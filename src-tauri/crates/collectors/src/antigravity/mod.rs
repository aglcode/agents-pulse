//! Antigravity collector (M6). **Read-only**: it reads Antigravity's conversation
//! databases for usage fields and model names. It never writes to, runs, or
//! reconfigures Antigravity, and never calls its language server. Sources and field
//! mapping are in `docs/providers/antigravity.md`.
//!
//! * Tokens come from `<data folder>/conversations/<id>.db` (Local, undocumented):
//!   one usage record per model call in `steps.metadata`. Generation records hold
//!   the same calls again, so they're used only to name the models.
//! * Context used is the latest model step's prompt size. The total is unavailable:
//!   Antigravity doesn't record its window, and its limits may differ from the
//!   model's own, so no total is guessed.
//! * Quota is unavailable: Antigravity exposes it only through its undocumented
//!   language server, which would need a CSRF token read from another process.
//! * Identity is install-scoped (no verified local source), one account per data
//!   folder. Scope `google:antigravity`, separate from Gemini CLI.
//!
//! Databases are read into memory and opened there (see `files` and `wal`), so
//! nothing is ever created in Antigravity's folders, and no copy of a conversation
//! is written anywhere.

mod files;
mod parse;
mod proto;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
#[cfg(test)]
mod tests;
mod wal;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agentpulse_core::batch::{
    Availability, AvailabilityUpdate, ContextObservation, CursorState, CursorUpdate, NormalizedBatch,
    SessionObservation, SessionStatus, UsageEvent,
};
use agentpulse_core::collector::{Capabilities, Collector, CollectorError, Ctx, Detection, InstallDetection, Trigger};
use agentpulse_core::hash::stable_hash;
use agentpulse_core::identity::IdentityEvidence;
use agentpulse_core::ids::{AgentId, CollectorId};
use agentpulse_core::metric::{SourceKind, Stability, UnavailableReason};
use agentpulse_core::time::{UnixMs, now_ms};
use agentpulse_core::usage::TokenUsage;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use files::{DbFile, ReadResult};
use parse::DbOutcome;

pub const COLLECTOR_ID: &str = "antigravity";
pub const SCOPE: &str = "google:antigravity";
/// Fallback poll; file watching is the primary trigger.
const INTERVAL: Duration = Duration::from_secs(300);
/// A conversation with activity this recent is shown as active.
const ACTIVE_WINDOW_MS: i64 = 5 * 60_000;
const SOURCE: SourceKind = SourceKind::Local;
const STABILITY: Stability = Stability::Undocumented;

/// One install's data folder, e.g. `~/.gemini/antigravity-ide`.
#[derive(Debug, Clone)]
struct Install {
    root: PathBuf,
    install_key: String,
}

pub struct AntigravityCollector {
    /// The folder holding Antigravity's data folders, normally `~/.gemini`.
    gemini_dir: PathBuf,
}

impl AntigravityCollector {
    pub fn new(gemini_dir: PathBuf) -> Self {
        Self { gemini_dir }
    }

    /// `<user home>/.gemini`.
    pub fn default_dir(user_home: &Path) -> PathBuf {
        user_home.join(".gemini")
    }

    /// Data folders that have a conversations folder.
    fn installs(&self) -> Vec<Install> {
        files::DATA_FOLDERS
            .iter()
            .map(|(name, _)| self.gemini_dir.join(name))
            .filter(|root| files::conversations_dir(root).is_dir())
            .map(|root| Install { install_key: stable_hash(&["antigravity-data", &normalize(&root)]), root })
            .collect()
    }
}

fn identity() -> IdentityEvidence {
    IdentityEvidence::none("no verified local identity source; install-scoped per data folder (docs/providers/antigravity.md)")
}

#[async_trait]
impl Collector for AntigravityCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new(COLLECTOR_ID)
    }

    fn agent(&self) -> AgentId {
        AgentId::Antigravity
    }

    fn capabilities(&self) -> Capabilities {
        let local = Some((SOURCE, STABILITY));
        Capabilities { metrics: vec![("tokens", local), ("context", local), ("quota", None)] }
    }

    fn triggers(&self) -> Vec<Trigger> {
        let watch = self.installs().iter().map(|i| files::conversations_dir(&i.root)).collect();
        vec![Trigger::WatchPaths(watch), Trigger::Interval(INTERVAL), Trigger::Manual]
    }

    fn timeout(&self) -> Duration {
        // The first run after enabling reads every conversation.
        Duration::from_secs(120)
    }

    async fn detect(&self, _ctx: &Ctx) -> Detection {
        let installs = self.installs();
        if installs.is_empty() {
            return Detection::NotInstalled;
        }
        Detection::Installed(
            installs
                .into_iter()
                .map(|i| InstallDetection { install_key: i.install_key, version: None, running: true, identity: identity() })
                .collect(),
        )
    }

    async fn collect(&self, ctx: &Ctx, cancel: &CancellationToken) -> Result<Vec<NormalizedBatch>, CollectorError> {
        let mut batches = Vec::new();
        for install in self.installs() {
            let (i, cursors, token) = (install.clone(), ctx.cursors.clone(), cancel.clone());
            let scan = tokio::task::spawn_blocking(move || scan(&i, &cursors, &token))
                .await
                .map_err(|_| CollectorError::Internal("antigravity scan task failed"))??;
            if scan.bad_rows + scan.corrupt + scan.unsupported + scan.unknown_format_rows > 0 {
                tracing::warn!(
                    collector = COLLECTOR_ID,
                    bad_rows = scan.bad_rows,
                    unknown_format_rows = scan.unknown_format_rows,
                    corrupt_files = scan.corrupt,
                    unsupported_files = scan.unsupported,
                    "skipped unreadable antigravity records"
                );
            }
            tracing::debug!(
                collector = COLLECTOR_ID,
                install = %&install.install_key[..12],
                files_read = scan.files_read,
                files_unchanged = scan.files_unchanged,
                files_busy = scan.files_busy,
                wal_frames = scan.wal_frames,
                usage = scan.usage.len(),
                unnamed_models = scan.unnamed,
                "antigravity scan done"
            );
            batches.push(build_batch(install.install_key, scan));
        }
        Ok(batches)
    }
}

fn build_batch(install_key: String, scan: Scan) -> NormalizedBatch {
    // Every database read this run had an unknown layout: say so instead of showing nothing.
    let tokens = if scan.files_read == 0 && scan.unsupported > 0 {
        Availability::Unavailable(UnavailableReason::UnsupportedVersion { detected: "conversation database schema".into() })
    } else {
        Availability::Available { source: SOURCE, stability: STABILITY }
    };
    NormalizedBatch {
        collector_id: CollectorId::new(COLLECTOR_ID),
        agent: AgentId::Antigravity,
        entitlement_scope: SCOPE.into(),
        install_key,
        identity: identity(),
        detected_version: None,
        collected_at: now_ms(),
        sessions: scan.sessions,
        usage: scan.usage,
        quotas: Vec::new(),
        contexts: scan.contexts,
        availability: vec![
            AvailabilityUpdate { metric: "tokens".into(), availability: tokens.clone() },
            AvailabilityUpdate { metric: "context".into(), availability: tokens },
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
    files_busy: u32,
    corrupt: u32,
    unsupported: u32,
    bad_rows: u32,
    unknown_format_rows: u32,
    wal_frames: usize,
    /// Usage records whose model has no name in their conversation.
    unnamed: u32,
}

/// Blocking: read every changed conversation database of one install.
fn scan(install: &Install, cursors: &HashMap<String, CursorState>, cancel: &CancellationToken) -> Result<Scan, CollectorError> {
    let key = install.install_key.as_str();
    let dbs = files::list(&install.root, key).map_err(|e| CollectorError::Io(e.kind()))?;
    let mut out = Scan::default();
    let mut usage_index: HashMap<String, usize> = HashMap::new();

    for f in &dbs {
        if cancel.is_cancelled() {
            return Err(CollectorError::Cancelled);
        }
        let unreadable = |e: std::io::Error| {
            // One unreadable file must not block the others. Log the hashed key only.
            tracing::warn!(collector = COLLECTOR_ID, source = %&f.source_key[..12], kind = ?e.kind(), "database unreadable");
        };
        let version = match files::current_version(f) {
            Ok(Some(v)) => v,
            Ok(None) => continue,
            Err(e) => {
                unreadable(e);
                continue;
            }
        };
        if cursors.get(&f.source_key).is_some_and(|c| c.file_id == version.id) {
            out.files_unchanged += 1;
            continue;
        }
        let snapshot = match files::read(f) {
            Ok(ReadResult::Read(s)) => s,
            Ok(ReadResult::Skipped) => {
                out.files_busy += 1;
                continue;
            }
            Err(e) => {
                unreadable(e);
                continue;
            }
        };
        let files::Snapshot { db: mut image, wal, version } = snapshot;
        if let wal::WalOutcome::Applied { frames } = wal::apply(&mut image, &wal) {
            out.wal_frames += frames;
        }
        drop(wal);
        let outcome = if wal::detach_wal_mode(&mut image) { parse::read_conversation(&image) } else { DbOutcome::Corrupt };
        drop(image);
        let cursor = CursorUpdate {
            source_key: f.source_key.clone(),
            state: CursorState { byte_offset: 0, file_id: version.id, size: version.size, mtime: version.mtime },
        };
        match outcome {
            DbOutcome::Read(conversation) => {
                out.files_read += 1;
                add_conversation(&mut out, &mut usage_index, key, f, conversation);
                out.cursors.push(cursor);
            }
            DbOutcome::Corrupt => {
                // Not retried until the file changes.
                out.corrupt += 1;
                out.cursors.push(cursor);
            }
            // Retried every run, so a later AgentPulse version that knows the layout reads it.
            DbOutcome::UnsupportedSchema => out.unsupported += 1,
        }
    }
    out.sessions.sort_by(|a, b| a.session_key.cmp(&b.session_key));
    out.contexts.sort_by_key(|c| c.observed_at);
    Ok(out)
}

fn add_conversation(
    out: &mut Scan,
    usage_index: &mut HashMap<String, usize>,
    install_key: &str,
    f: &DbFile,
    c: parse::Conversation,
) {
    out.bad_rows += c.bad_rows;
    out.unknown_format_rows += c.unknown_format_rows;
    let session_key = stable_hash(&["antigravity-session", install_key, &f.conversation]);
    let mut span: Option<(UnixMs, UnixMs)> = None;
    // Latest model step of the conversation itself: (time, context used, model).
    let mut latest: Option<(UnixMs, u64, Option<String>)> = None;

    for step in c.steps {
        let Some(at) = step.created_at else {
            // A record without a time can't be placed; it's never given a guessed one.
            if step.usage.is_some() {
                out.bad_rows += 1;
            }
            continue;
        };
        span = Some(span.map_or((at, at), |(s, e)| (s.min(at), e.max(at))));
        let Some(u) = step.usage else {
            continue;
        };
        let model = u.model.and_then(|n| c.names.get(&n).cloned());
        if model.is_none() {
            out.unnamed += 1;
        }
        let id = match &u.response_id {
            Some(r) => stable_hash(&["antigravity-call", install_key, r]),
            None => stable_hash(&["antigravity-step", install_key, &f.conversation, &step.idx.to_string()]),
        };
        // Auxiliary calls (no generator model) count as usage but don't say how full
        // the conversation's context is.
        if step.generator_model.is_some() && latest.as_ref().is_none_or(|(t, _, _)| at >= *t) {
            latest = Some((at, u.tokens.context_used(), model.clone()));
        }
        let t = u.tokens;
        let event = UsageEvent {
            dedup_key: format!("antigravity:{}", &id[..32]),
            // A record can be updated in place while a reply streams; larger totals win.
            source_seq: i64::try_from(t.sum()).unwrap_or(i64::MAX),
            session_key: Some(session_key.clone()),
            model,
            occurred_at: at,
            tokens: TokenUsage {
                input: Some(t.input),
                output: Some(t.output),
                cache_read: Some(t.cache_read),
                // Zero is omitted on the wire, so an absent field can't be told from 0.
                // Only nonzero values are recorded for fields some models never report.
                cache_write: (t.cache_write > 0).then_some(t.cache_write),
                reasoning: (t.thinking > 0).then_some(t.thinking),
            },
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

    let Some((started_at, last_activity_at)) = span else {
        return;
    };
    let now = now_ms();
    out.sessions.push(SessionObservation {
        session_key: session_key.clone(),
        model: latest.as_ref().and_then(|(_, _, m)| m.clone()),
        started_at,
        last_activity_at,
        ended_at: None,
        status: if now - last_activity_at < ACTIVE_WINDOW_MS { SessionStatus::Active } else { SessionStatus::Idle },
    });
    if let Some((at, used, _)) = latest {
        out.contexts.push(ContextObservation {
            session_key,
            used_tokens: used,
            total_tokens: None,
            total_source: SourceKind::Unavailable,
            observed_at: at,
            source: SOURCE,
            stability: STABILITY,
        });
    }
}

/// Comparable form of a path: forward slashes, no trailing slash, and
/// case-insensitive on Windows.
fn normalize(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/');
    if cfg!(windows) { s.to_lowercase() } else { s.to_owned() }
}
