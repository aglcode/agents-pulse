//! Gemini CLI collector (M5). **Read-only**: it reads Gemini CLI's own session logs
//! for usage fields, and the sign-in and auth type from its settings. It never
//! writes to, runs, or reconfigures Gemini CLI. Sources and field mapping are in
//! `docs/providers/gemini_cli.md`.
//!
//! * Tokens and context come from `~/.gemini/tmp/<project>/chats/*.jsonl` (Local,
//!   undocumented). The context total is Estimated from a model table.
//! * Quota is unavailable: Gemini CLI exposes it only through an internal Google API
//!   that would need the user's OAuth tokens.
//! * Telemetry is detected but not read: its file format is unverified, and it
//!   records the same API calls as the session logs (summing both would double-count).
//!
//! Never reads `oauth_creds.json`, `tmp/*/logs.json` or `tmp/*/.project_root`.

mod files;
mod identity;
mod parse;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use agentpulse_core::batch::{
    Availability, AvailabilityUpdate, ContextObservation, CursorState, CursorUpdate, NormalizedBatch,
    SessionObservation, SessionStatus, UsageEvent,
};
use agentpulse_core::collector::{Capabilities, Collector, CollectorError, Ctx, Detection, InstallDetection, Trigger};
use agentpulse_core::hash::stable_hash;
use agentpulse_core::ids::{AgentId, CollectorId};
use agentpulse_core::metric::{SourceKind, Stability, UnavailableReason};
use agentpulse_core::time::{UnixMs, now_ms};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use files::ReadOutcome;
pub use identity::TelemetryState;
use identity::{Fingerprint, Signin};

pub const COLLECTOR_ID: &str = "gemini-cli";
/// Fallback poll; file watching is the primary trigger.
const INTERVAL: Duration = Duration::from_secs(300);
/// A session with activity this recent is shown as active.
const ACTIVE_WINDOW_MS: i64 = 5 * 60_000;
const SOURCE: SourceKind = SourceKind::Local;
const STABILITY: Stability = Stability::Undocumented;

pub struct GeminiCliCollector {
    /// Gemini CLI's home, normally `~/.gemini`.
    home: PathBuf,
    /// Sign-in, re-read only when a settings file changes.
    signin: Mutex<Option<(Fingerprint, Signin)>>,
}

impl GeminiCliCollector {
    pub fn new(home: PathBuf) -> Self {
        Self { home, signin: Mutex::new(None) }
    }

    /// `<user home>/.gemini`.
    pub fn default_home(user_home: &Path) -> PathBuf {
        user_home.join(".gemini")
    }

    fn install_key(&self) -> String {
        stable_hash(&["gemini-home", &normalize(&self.home)])
    }

    fn signin(&self) -> Signin {
        let print = identity::fingerprint(&self.home);
        if let Ok(cache) = self.signin.lock()
            && let Some((cached, s)) = cache.as_ref()
            && *cached == print
        {
            return s.clone();
        }
        let s = identity::read(&self.home);
        if let Ok(mut cache) = self.signin.lock() {
            *cache = Some((print, s.clone()));
        }
        s
    }

    /// Gemini CLI's own telemetry setting, for the dashboard. Reads `settings.json` only.
    pub fn telemetry(&self) -> TelemetryState {
        self.signin().telemetry
    }
}

#[async_trait]
impl Collector for GeminiCliCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new(COLLECTOR_ID)
    }

    fn agent(&self) -> AgentId {
        AgentId::GeminiCli
    }

    fn capabilities(&self) -> Capabilities {
        let local = Some((SOURCE, STABILITY));
        Capabilities { metrics: vec![("tokens", local), ("context", local), ("quota", None)] }
    }

    fn triggers(&self) -> Vec<Trigger> {
        let tmp = files::tmp_dir(&self.home);
        let watch = if tmp.is_dir() { vec![tmp] } else { Vec::new() };
        vec![Trigger::WatchPaths(watch), Trigger::Interval(INTERVAL), Trigger::Manual]
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
            identity: self.signin().identity,
        }])
    }

    async fn collect(&self, ctx: &Ctx, cancel: &CancellationToken) -> Result<Vec<NormalizedBatch>, CollectorError> {
        if !files::is_installed(&self.home) {
            return Ok(Vec::new());
        }
        let signin = self.signin();
        let install_key = self.install_key();
        let (home, key, cursors, token) = (self.home.clone(), install_key.clone(), ctx.cursors.clone(), cancel.clone());
        let scan = tokio::task::spawn_blocking(move || scan(&home, &key, &cursors, &token))
            .await
            .map_err(|_| CollectorError::Internal("gemini cli scan task failed"))??;
        if scan.bad_lines > 0 {
            tracing::debug!(collector = COLLECTOR_ID, bad_lines = scan.bad_lines, "skipped unparseable log lines");
        }
        tracing::debug!(
            collector = COLLECTOR_ID,
            files_read = scan.files_read,
            files_unchanged = scan.files_unchanged,
            usage = scan.usage.len(),
            "gemini cli scan done"
        );
        Ok(vec![build_batch(install_key, signin, scan)])
    }
}

fn build_batch(install_key: String, signin: Signin, scan: Scan) -> NormalizedBatch {
    let available = Availability::Available { source: SOURCE, stability: STABILITY };
    NormalizedBatch {
        collector_id: CollectorId::new(COLLECTOR_ID),
        agent: AgentId::GeminiCli,
        entitlement_scope: signin.scope.into(),
        install_key,
        identity: signin.identity,
        detected_version: None,
        collected_at: now_ms(),
        sessions: scan.sessions,
        usage: scan.usage,
        quotas: Vec::new(),
        contexts: scan.contexts,
        availability: vec![
            AvailabilityUpdate { metric: "tokens".into(), availability: available.clone() },
            AvailabilityUpdate { metric: "context".into(), availability: available },
            AvailabilityUpdate {
                metric: "quota".into(),
                availability: Availability::Unavailable(UnavailableReason::NotExposedByProvider),
            },
        ],
        cursors: scan.cursors,
    }
}

#[derive(Default)]
struct Scan {
    usage: Vec<UsageEvent>,
    sessions: Vec<SessionObservation>,
    contexts: Vec<ContextObservation>,
    cursors: Vec<CursorUpdate>,
    files_read: u32,
    files_unchanged: u32,
    bad_lines: u32,
}

/// Per-session aggregate across every file read in this run.
struct SessionAgg {
    started_at: UnixMs,
    last_activity_at: UnixMs,
    /// Latest main-thread (non-subagent) reply: (time, model).
    model: Option<(UnixMs, Option<String>)>,
    /// Latest main-thread reply: (time, context used, model).
    context: Option<(UnixMs, u64, Option<String>)>,
}

/// Blocking: read every changed session log.
fn scan(
    home: &Path,
    install_key: &str,
    cursors: &HashMap<String, CursorState>,
    cancel: &CancellationToken,
) -> Result<Scan, CollectorError> {
    let logs = files::list(home, install_key).map_err(|e| CollectorError::Io(e.kind()))?;
    let mut out = Scan::default();
    let mut sessions: HashMap<String, SessionAgg> = HashMap::new();
    let mut usage_index: HashMap<String, usize> = HashMap::new();

    for log in &logs {
        if cancel.is_cancelled() {
            return Err(CollectorError::Cancelled);
        }
        let outcome = match files::read(log, cursors) {
            Ok(o) => o,
            Err(e) => {
                // One unreadable file must not block the others. Log the hashed key only.
                tracing::warn!(collector = COLLECTOR_ID, source = %&log.source_key[..12], kind = ?e.kind(), "log unreadable");
                continue;
            }
        };
        let ReadOutcome::Read { read, cursor } = outcome else {
            out.files_unchanged += 1;
            continue;
        };
        out.files_read += 1;
        out.bad_lines += read.bad_lines;

        // Message lines carry no session id; it's in the header. Without one, the
        // file itself stands in for the session (its key is already hashed).
        let header = files::header(log);
        let own_session = header.as_ref().map_or_else(|| log.source_key.clone(), |h| h.session_id.clone());
        let subagent = log.parent_session.is_some() || header.as_ref().is_some_and(|h| h.subagent);
        // A subagent's usage counts toward the session that started it.
        let session_id = log.parent_session.clone().unwrap_or_else(|| own_session.clone());
        let session_key = stable_hash(&["gemini-session", install_key, &session_id]);
        let message_scope = &stable_hash(&["gemini-msg", install_key, &own_session])[..16];

        for (line, offset) in read.items.into_iter().zip(read.offsets) {
            let at = line.occurred_at;
            let agg = sessions.entry(session_key.clone()).or_insert(SessionAgg {
                started_at: at,
                last_activity_at: at,
                model: None,
                context: None,
            });
            agg.started_at = agg.started_at.min(at);
            agg.last_activity_at = agg.last_activity_at.max(at);
            if !subagent {
                if agg.model.as_ref().is_none_or(|(t, _)| at >= *t) {
                    agg.model = Some((at, line.model.clone()));
                }
                if agg.context.as_ref().is_none_or(|(t, _, _)| at >= *t) {
                    agg.context = Some((at, line.context_used, line.model.clone()));
                }
            }
            let event = UsageEvent {
                dedup_key: format!("gemini:{message_scope}:{}", line.message_id),
                // A message is rewritten as it changes; the later line wins.
                source_seq: i64::try_from(offset).unwrap_or(i64::MAX),
                session_key: Some(session_key.clone()),
                model: line.model,
                occurred_at: at,
                tokens: line.tokens,
                source: SOURCE,
                stability: STABILITY,
            };
            // Collapse repeats within this run; the store handles repeats across runs.
            match usage_index.get(&event.dedup_key) {
                Some(&i) if out.usage[i].source_seq >= event.source_seq => {}
                Some(&i) => out.usage[i] = event,
                None => {
                    usage_index.insert(event.dedup_key.clone(), out.usage.len());
                    out.usage.push(event);
                }
            }
        }
        out.cursors.push(cursor);
    }

    let now = now_ms();
    for (key, agg) in sessions {
        out.sessions.push(SessionObservation {
            session_key: key.clone(),
            model: agg.model.and_then(|(_, m)| m),
            started_at: agg.started_at,
            last_activity_at: agg.last_activity_at,
            ended_at: None,
            status: if now - agg.last_activity_at < ACTIVE_WINDOW_MS { SessionStatus::Active } else { SessionStatus::Idle },
        });
        if let Some((at, used, model)) = agg.context {
            let total = model.as_deref().and_then(parse::estimated_context_total);
            out.contexts.push(ContextObservation {
                session_key: key,
                used_tokens: used,
                total_tokens: total,
                // Unknown model: no total, rather than a guess.
                total_source: if total.is_some() { SourceKind::Estimated } else { SourceKind::Unavailable },
                observed_at: at,
                source: SOURCE,
                stability: STABILITY,
            });
        }
    }
    out.sessions.sort_by(|a, b| a.session_key.cmp(&b.session_key));
    out.contexts.sort_by_key(|c| c.observed_at);
    Ok(out)
}

/// Comparable form of a path: forward slashes, no trailing slash, and
/// case-insensitive on Windows.
fn normalize(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/');
    if cfg!(windows) { s.to_lowercase() } else { s.to_owned() }
}
