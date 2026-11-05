//! Codex collector (M3). Reads Codex's local rollout logs only: no config
//! changes, no network, no subprocesses. Verified sources and field mapping are
//! in `docs/providers/codex.md`.
//!
//! Never reads `~/.codex/auth.json` or any other file outside the rollout folders.

mod files;
mod parse;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use agentpulse_core::batch::{
    Availability, AvailabilityUpdate, CursorState, NormalizedBatch, QuotaObservation, SessionObservation, SessionStatus,
};
use agentpulse_core::collector::{Capabilities, Collector, CollectorError, Ctx, Detection, InstallDetection, Trigger};
use agentpulse_core::hash::stable_hash;
use agentpulse_core::identity::IdentityEvidence;
use agentpulse_core::ids::{AgentId, CollectorId};
use agentpulse_core::metric::{SourceKind, Stability};
use agentpulse_core::time::{UnixMs, now_ms};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use files::ReadOutcome;

pub const COLLECTOR_ID: &str = "codex";
const SCOPE: &str = "openai:chatgpt_codex";
/// Fallback poll; file watching is the primary trigger.
const INTERVAL: Duration = Duration::from_secs(300);
/// A session with activity this recent is shown as active.
const ACTIVE_WINDOW_MS: i64 = 5 * 60_000;

#[derive(Default)]
struct State {
    /// Latest ChatGPT plan type seen in rollouts (for the account label).
    plan: Option<(UnixMs, String)>,
    /// Whether rate limits have ever been observed for this install.
    quota_seen: bool,
}

pub struct CodexCollector {
    home: PathBuf,
    state: Mutex<State>,
}

impl CodexCollector {
    /// `home` is the Codex home directory (see [`CodexCollector::default_home`]).
    pub fn new(home: PathBuf) -> Self {
        Self { home, state: Mutex::new(State::default()) }
    }

    /// `$CODEX_HOME` if set, otherwise `<user home>/.codex` (Codex's documented default).
    pub fn default_home(user_home: &Path) -> PathBuf {
        std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| user_home.join(".codex"))
    }

    fn install_key(&self) -> String {
        stable_hash(&["codex-home", &self.home.to_string_lossy()])
    }

    fn identity(&self) -> IdentityEvidence {
        let plan = self.state.lock().ok().and_then(|s| s.plan.clone());
        match plan {
            // Auth mode only: rate limits with a plan type exist only for ChatGPT logins.
            // The account stays install-scoped (no verified identity; see codex.md).
            Some((_, plan)) => IdentityEvidence::auth_mode_only(
                format!("ChatGPT {}", title_case(&plan)),
                "codex rollout rate_limits.plan_type",
            ),
            None => IdentityEvidence::none("codex rollouts (no plan seen yet)"),
        }
    }
}

#[async_trait]
impl Collector for CodexCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new(COLLECTOR_ID)
    }

    fn agent(&self) -> AgentId {
        AgentId::Codex
    }

    fn capabilities(&self) -> Capabilities {
        let local = Some((SourceKind::Local, Stability::Undocumented));
        Capabilities { metrics: vec![("tokens", local), ("quota", local), ("context", local)] }
    }

    fn triggers(&self) -> Vec<Trigger> {
        vec![Trigger::WatchPaths(files::watch_dirs(&self.home)), Trigger::Interval(INTERVAL), Trigger::Manual]
    }

    fn timeout(&self) -> Duration {
        // The first run after enabling backfills all history.
        Duration::from_secs(120)
    }

    async fn detect(&self, _ctx: &Ctx) -> Detection {
        if !files::is_installed(&self.home) {
            return Detection::NotInstalled;
        }
        Detection::Installed(vec![InstallDetection {
            install_key: self.install_key(),
            version: None,
            running: true,
            identity: self.identity(),
        }])
    }

    async fn collect(&self, ctx: &Ctx, cancel: &CancellationToken) -> Result<Vec<NormalizedBatch>, CollectorError> {
        let home = self.home.clone();
        let cursors = ctx.cursors.clone();
        let cancel = cancel.clone();
        let need_plan = self.state.lock().map(|s| s.plan.is_none()).unwrap_or(true);
        let result = tokio::task::spawn_blocking(move || scan_all(&home, &cursors, &cancel, need_plan))
            .await
            .map_err(|_| CollectorError::Internal("codex scan task failed"))??;

        if let Ok(mut s) = self.state.lock() {
            if let Some(p) = result.plan.clone()
                && s.plan.as_ref().is_none_or(|(t, _)| p.0 >= *t)
            {
                s.plan = Some(p);
            }
            s.quota_seen |= result.quota_seen;
        }
        if result.bad_lines > 0 {
            tracing::debug!(
                collector = COLLECTOR_ID,
                bad_lines = result.bad_lines,
                "skipped unparseable rollout lines"
            );
        }
        tracing::debug!(
            collector = COLLECTOR_ID,
            files_read = result.files_read,
            files_unchanged = result.files_unchanged,
            usage = result.batch_usage_len(),
            "codex scan done"
        );
        Ok(vec![self.build_batch(result)])
    }
}

impl CodexCollector {
    fn build_batch(&self, r: ScanResult) -> NormalizedBatch {
        let quota_seen = self.state.lock().map(|s| s.quota_seen).unwrap_or(false);
        let available = Availability::Available { source: SourceKind::Local, stability: Stability::Undocumented };
        let mut availability = vec![
            AvailabilityUpdate { metric: "tokens".into(), availability: available.clone() },
            AvailabilityUpdate { metric: "context".into(), availability: available.clone() },
        ];
        if quota_seen {
            availability.push(AvailabilityUpdate { metric: "quota".into(), availability: available });
        }
        NormalizedBatch {
            collector_id: CollectorId::new(COLLECTOR_ID),
            agent: AgentId::Codex,
            entitlement_scope: SCOPE.into(),
            install_key: self.install_key(),
            identity: self.identity(),
            detected_version: r.version,
            collected_at: now_ms(),
            sessions: r.sessions,
            usage: r.usage,
            quotas: r.quotas,
            contexts: r.contexts,
            availability,
            cursors: r.cursors,
        }
    }
}

#[derive(Default)]
struct ScanResult {
    usage: Vec<agentpulse_core::batch::UsageEvent>,
    quotas: Vec<QuotaObservation>,
    contexts: Vec<agentpulse_core::batch::ContextObservation>,
    sessions: Vec<SessionObservation>,
    cursors: Vec<agentpulse_core::batch::CursorUpdate>,
    plan: Option<(UnixMs, String)>,
    version: Option<String>,
    quota_seen: bool,
    files_read: u32,
    files_unchanged: u32,
    bad_lines: u32,
}

impl ScanResult {
    fn batch_usage_len(&self) -> usize {
        self.usage.len()
    }
}

/// Blocking: walk all rollouts, read changed ones, assemble one batch's worth of data.
fn scan_all(
    home: &Path,
    cursors: &HashMap<String, CursorState>,
    cancel: &CancellationToken,
    need_plan: bool,
) -> Result<ScanResult, CollectorError> {
    let rollouts = files::list(home).map_err(|e| CollectorError::Io(e.kind()))?;
    let now = now_ms();
    let mut r = ScanResult::default();
    let mut latest_version: Option<(UnixMs, String)> = None;

    for rollout in &rollouts {
        if cancel.is_cancelled() {
            return Err(CollectorError::Cancelled);
        }
        let outcome = match files::read(rollout, cursors) {
            Ok(o) => o,
            Err(e) => {
                // One unreadable file must not block the others. Log the hashed key only.
                tracing::warn!(collector = COLLECTOR_ID, source = %&rollout.source_key[..12], kind = ?e.kind(), "rollout unreadable");
                continue;
            }
        };
        let ReadOutcome::Scanned { scan, cursor } = outcome else {
            r.files_unchanged += 1;
            continue;
        };
        let scan = *scan;
        r.files_read += 1;
        r.bad_lines += scan.bad_lines;
        merge_plan(&mut r.plan, scan.plan.clone());
        if let (Some(v), Some(at)) = (&scan.cli_version, scan.last_activity_at)
            && latest_version.as_ref().is_none_or(|(t, _)| at >= *t)
        {
            latest_version = Some((at, v.clone()));
        }
        if scan.emitted
            && let Some(last) = scan.last_activity_at
        {
            r.sessions.push(SessionObservation {
                session_key: scan.session_key.clone(),
                model: scan.model.clone(),
                started_at: scan.started_at.unwrap_or(last),
                last_activity_at: last,
                ended_at: None,
                status: if now - last < ACTIVE_WINDOW_MS { SessionStatus::Active } else { SessionStatus::Idle },
            });
        }
        r.quota_seen |= !scan.quotas.is_empty();
        r.usage.extend(scan.usage);
        r.quotas.extend(scan.quotas);
        r.contexts.extend(scan.context);
        r.cursors.push(cursor);
    }

    // After a restart nothing may have changed; still learn the plan for the account label
    // from the newest rollout (read-only, nothing emitted).
    if need_plan
        && r.plan.is_none()
        && let Some(newest) = rollouts.iter().max_by_key(|f| std::fs::metadata(&f.path).and_then(|m| m.modified()).ok())
        && let Ok(ReadOutcome::Scanned { scan, .. }) = files::read(newest, &HashMap::new())
    {
        merge_plan(&mut r.plan, scan.plan);
        r.quota_seen |= !scan.quotas.is_empty();
    }

    r.version = latest_version.map(|(_, v)| v);
    r.quotas = changes_only(std::mem::take(&mut r.quotas));
    Ok(r)
}

fn merge_plan(into: &mut Option<(UnixMs, String)>, candidate: Option<(UnixMs, String)>) {
    if let Some(c) = candidate
        && into.as_ref().is_none_or(|(t, _)| c.0 >= *t)
    {
        *into = Some(c);
    }
}

/// Sort quota readings by time and keep, per window, only readings where the value
/// or reset time changed, plus the last one (which extends the current segment).
/// Concurrent sessions report the same account limits, so this also removes
/// cross-session duplicates.
fn changes_only(mut obs: Vec<QuotaObservation>) -> Vec<QuotaObservation> {
    obs.sort_by_key(|q| q.observed_at);
    let mut last_kept: HashMap<String, (f64, Option<UnixMs>)> = HashMap::new();
    let mut last_index: HashMap<String, usize> = HashMap::new();
    for (i, q) in obs.iter().enumerate() {
        last_index.insert(q.window_kind.clone(), i);
    }
    let finals: HashSet<usize> = last_index.into_values().collect();
    let mut out = Vec::new();
    for (i, q) in obs.into_iter().enumerate() {
        let changed = last_kept
            .get(&q.window_kind)
            .is_none_or(|(pct, resets)| (pct - q.used_pct).abs() > 1e-9 || *resets != q.resets_at);
        if changed || finals.contains(&i) {
            last_kept.insert(q.window_kind.clone(), (q.used_pct, q.resets_at));
            out.push(q);
        }
    }
    out
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}
