//! Claude Code collector (M4). **Read-only**: it reads session logs and the
//! sign-in recorded in Claude Code's settings file, and never writes to, runs, or
//! reconfigures Claude Code. Sources and field mapping are in
//! `docs/providers/claude_code.md`.
//!
//! One Claude Code *config folder* is one installation: the default `~/.claude`,
//! the folder named by `CLAUDE_CONFIG_DIR` (if AgentPulse can see it), and any
//! folder the user adds. Each folder gets its own batch, identity and account, so
//! two folders signed in to different Claude accounts never share usage.
//!
//! Never reads `.credentials.json`.

mod files;
mod identity;
mod parse;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, SystemTime};

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
use identity::Signin;

pub const COLLECTOR_ID: &str = "claude-code";
/// Fallback poll; file watching is the primary trigger.
const INTERVAL: Duration = Duration::from_secs(300);
/// A session with activity this recent is shown as active.
const ACTIVE_WINDOW_MS: i64 = 5 * 60_000;
const SOURCE: SourceKind = SourceKind::Local;
const STABILITY: Stability = Stability::Undocumented;

/// How a config folder came to be collected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderOrigin {
    /// `<home>/.claude`, Claude Code's default.
    Default,
    /// `CLAUDE_CONFIG_DIR` as seen by AgentPulse's own process.
    Environment,
    /// Added by the user in AgentPulse.
    Added,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFolder {
    pub path: PathBuf,
    pub origin: FolderOrigin,
}

impl ConfigFolder {
    pub fn has_logs(&self) -> bool {
        files::has_logs_folder(&self.path)
    }

    fn install_key(&self) -> String {
        stable_hash(&["claude-config-dir", &normalize(&self.path)])
    }
}

/// Cached sign-in per folder, re-read only when the settings file changes.
type SigninCache = HashMap<String, (Option<(PathBuf, SystemTime, u64)>, Signin)>;

pub struct ClaudeCodeCollector {
    user_home: PathBuf,
    env_dir: Option<PathBuf>,
    folders: RwLock<Vec<ConfigFolder>>,
    signins: Mutex<SigninCache>,
}

impl ClaudeCodeCollector {
    /// `env_dir` is `CLAUDE_CONFIG_DIR` (see [`ClaudeCodeCollector::env_config_dir`]);
    /// `added` are folders the user added in AgentPulse.
    pub fn new(user_home: PathBuf, env_dir: Option<PathBuf>, added: Vec<PathBuf>) -> Self {
        let c = Self { user_home, env_dir, folders: RwLock::new(Vec::new()), signins: Mutex::new(HashMap::new()) };
        c.set_added_folders(added);
        c
    }

    /// `CLAUDE_CONFIG_DIR` from AgentPulse's own environment. Often unset even when
    /// the user's terminals set it, which is why folders can also be added by hand.
    pub fn env_config_dir() -> Option<PathBuf> {
        std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()).map(PathBuf::from)
    }

    pub fn default_folder(&self) -> PathBuf {
        self.user_home.join(".claude")
    }

    /// Every folder collected, in order: default, environment, then user-added. Duplicates removed.
    pub fn folders(&self) -> Vec<ConfigFolder> {
        self.folders.read().map(|f| f.clone()).unwrap_or_default()
    }

    /// Replace the user-added folders. The caller restarts the worker so file
    /// watching follows the new set.
    pub fn set_added_folders(&self, added: Vec<PathBuf>) {
        let mut list = vec![ConfigFolder { path: self.default_folder(), origin: FolderOrigin::Default }];
        if let Some(env) = &self.env_dir {
            list.push(ConfigFolder { path: env.clone(), origin: FolderOrigin::Environment });
        }
        list.extend(added.into_iter().map(|path| ConfigFolder { path, origin: FolderOrigin::Added }));
        let mut seen = std::collections::HashSet::new();
        list.retain(|f| seen.insert(normalize(&f.path)));
        if let Ok(mut f) = self.folders.write() {
            *f = list;
        }
    }

    /// Whether `path` is already collected.
    pub fn is_collected(&self, path: &Path) -> bool {
        let n = normalize(path);
        self.folders().iter().any(|f| normalize(&f.path) == n)
    }

    /// Folders in the user's home that look like extra Claude Code config folders
    /// (`.claude-*` with a `projects` log folder) and aren't collected yet.
    /// Offered to the user; never added automatically.
    pub fn suggestions(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.user_home) else { return Vec::new() };
        let mut out: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter(|e| e.file_name().to_string_lossy().starts_with(".claude"))
            .map(|e| e.path())
            .filter(|p| files::has_logs_folder(p) && !self.is_collected(p))
            .collect();
        out.sort();
        out
    }

    fn signin(&self, folder: &ConfigFolder) -> Signin {
        let settings = identity::settings_files(&folder.path, folder.origin == FolderOrigin::Default, &self.user_home);
        let print = identity::fingerprint(&settings);
        let key = folder.install_key();
        if let Ok(cache) = self.signins.lock()
            && let Some((cached_print, signin)) = cache.get(&key)
            && *cached_print == print
        {
            return signin.clone();
        }
        let signin = identity::read(&settings);
        if let Ok(mut cache) = self.signins.lock() {
            cache.insert(key, (print, signin.clone()));
        }
        signin
    }

    fn active_folders(&self) -> Vec<ConfigFolder> {
        self.folders().into_iter().filter(ConfigFolder::has_logs).collect()
    }
}

#[async_trait]
impl Collector for ClaudeCodeCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new(COLLECTOR_ID)
    }

    fn agent(&self) -> AgentId {
        AgentId::ClaudeCode
    }

    fn capabilities(&self) -> Capabilities {
        let local = Some((SOURCE, STABILITY));
        // Quota exists only through a status line command, which would mean editing
        // Claude Code's settings: not offered (read-only).
        Capabilities { metrics: vec![("tokens", local), ("context", local), ("quota", None)] }
    }

    fn triggers(&self) -> Vec<Trigger> {
        let dirs = self.folders().iter().map(|f| files::projects_dir(&f.path)).collect();
        vec![Trigger::WatchPaths(dirs), Trigger::Interval(INTERVAL), Trigger::Manual]
    }

    fn timeout(&self) -> Duration {
        // The first run after enabling backfills all history (hundreds of MB is normal).
        Duration::from_secs(300)
    }

    async fn detect(&self, _ctx: &Ctx) -> Detection {
        let folders = self.active_folders();
        if folders.is_empty() {
            return Detection::NotInstalled;
        }
        Detection::Installed(
            folders
                .iter()
                .map(|f| InstallDetection {
                    install_key: f.install_key(),
                    version: None,
                    running: true,
                    identity: self.signin(f).identity,
                })
                .collect(),
        )
    }

    async fn collect(&self, ctx: &Ctx, cancel: &CancellationToken) -> Result<Vec<NormalizedBatch>, CollectorError> {
        let mut batches = Vec::new();
        for folder in self.active_folders() {
            if cancel.is_cancelled() {
                return Err(CollectorError::Cancelled);
            }
            let signin = self.signin(&folder);
            let install_key = folder.install_key();
            let (path, key, cursors, token) = (folder.path.clone(), install_key.clone(), ctx.cursors.clone(), cancel.clone());
            let scan = tokio::task::spawn_blocking(move || scan_folder(&path, &key, &cursors, &token))
                .await
                .map_err(|_| CollectorError::Internal("claude code scan task failed"))??;
            if scan.bad_lines > 0 {
                tracing::debug!(collector = COLLECTOR_ID, bad_lines = scan.bad_lines, "skipped unparseable log lines");
            }
            tracing::debug!(
                collector = COLLECTOR_ID,
                folder = %&install_key[..12],
                files_read = scan.files_read,
                files_unchanged = scan.files_unchanged,
                usage = scan.usage.len(),
                "claude code scan done"
            );
            batches.push(build_batch(install_key, signin, scan));
        }
        Ok(batches)
    }
}

fn build_batch(install_key: String, signin: Signin, scan: FolderScan) -> NormalizedBatch {
    let available = Availability::Available { source: SOURCE, stability: STABILITY };
    NormalizedBatch {
        collector_id: CollectorId::new(COLLECTOR_ID),
        agent: AgentId::ClaudeCode,
        entitlement_scope: signin.scope.into(),
        install_key,
        identity: signin.identity,
        detected_version: scan.version,
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
                availability: Availability::Unavailable(UnavailableReason::RequiresToolSettingsChange),
            },
        ],
        cursors: scan.cursors,
    }
}

#[derive(Default)]
struct FolderScan {
    usage: Vec<UsageEvent>,
    sessions: Vec<SessionObservation>,
    contexts: Vec<ContextObservation>,
    cursors: Vec<CursorUpdate>,
    version: Option<String>,
    files_read: u32,
    files_unchanged: u32,
    bad_lines: u32,
}

/// Per-session aggregate across every file read in this run.
struct SessionAgg {
    started_at: UnixMs,
    last_activity_at: UnixMs,
    model: Option<(UnixMs, String)>,
    /// Latest main-thread (non-subagent) request: (time, context used).
    context: Option<(UnixMs, u64)>,
}

/// Blocking: read every changed log in one config folder.
fn scan_folder(
    folder: &Path,
    install_key: &str,
    cursors: &HashMap<String, CursorState>,
    cancel: &CancellationToken,
) -> Result<FolderScan, CollectorError> {
    let logs = files::list(folder, install_key).map_err(|e| CollectorError::Io(e.kind()))?;
    let mut out = FolderScan::default();
    let mut sessions: HashMap<String, SessionAgg> = HashMap::new();
    let mut latest_version: Option<(UnixMs, String)> = None;
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
        // Session key for lines without a session id: the file itself.
        let file_session = stable_hash(&["claude-session-file", &log.source_key]);
        for (line, offset) in read.lines.into_iter().zip(read.offsets) {
            let session_key = match &line.session_id {
                Some(id) => stable_hash(&["claude-session", install_key, id]),
                None => file_session.clone(),
            };
            let at = line.occurred_at;
            if let Some(v) = &line.version
                && latest_version.as_ref().is_none_or(|(t, _)| at >= *t)
            {
                latest_version = Some((at, v.clone()));
            }
            let agg = sessions.entry(session_key.clone()).or_insert(SessionAgg {
                started_at: at,
                last_activity_at: at,
                model: None,
                context: None,
            });
            agg.started_at = agg.started_at.min(at);
            agg.last_activity_at = agg.last_activity_at.max(at);
            if !line.sidechain {
                if let Some(m) = &line.model
                    && agg.model.as_ref().is_none_or(|(t, _)| at >= *t)
                {
                    agg.model = Some((at, m.clone()));
                }
                if agg.context.is_none_or(|(t, _)| at >= t) {
                    agg.context = Some((at, line.context_used));
                }
            }
            let event = UsageEvent {
                dedup_key: format!("claude:{}", line.message_id),
                // A message is written once per content block; the later line has the
                // fuller usage (output_tokens only ever grows), so it wins.
                source_seq: i64::try_from(offset).unwrap_or(i64::MAX),
                session_key: Some(session_key),
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
            model: agg.model.map(|(_, m)| m),
            started_at: agg.started_at,
            last_activity_at: agg.last_activity_at,
            ended_at: None,
            status: if now - agg.last_activity_at < ACTIVE_WINDOW_MS { SessionStatus::Active } else { SessionStatus::Idle },
        });
        if let Some((at, used)) = agg.context {
            out.contexts.push(ContextObservation {
                session_key: key,
                used_tokens: used,
                total_tokens: Some(parse::estimated_context_total(used)),
                total_source: SourceKind::Estimated,
                observed_at: at,
                source: SOURCE,
                stability: STABILITY,
            });
        }
    }
    out.sessions.sort_by(|a, b| a.session_key.cmp(&b.session_key));
    out.contexts.sort_by_key(|c| c.observed_at);
    out.version = latest_version.map(|(_, v)| v);
    Ok(out)
}

/// Comparable form of a folder path: forward slashes, no trailing slash, and
/// case-insensitive on Windows.
fn normalize(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/');
    if cfg!(windows) { s.to_lowercase() } else { s.to_owned() }
}
