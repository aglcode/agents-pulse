//! The quota tap: what runs when Claude Code calls AgentPulse as its status line.
//!
//! Claude Code passes a documented JSON packet on stdin
//! (<https://code.claude.com/docs/en/statusline>). The tap keeps **only** the
//! rate-limit windows, the context window numbers and a hashed session id, writes
//! them to one small file in AgentPulse's own data folder (overwritten every time),
//! and prints a short status text. Paths (`cwd`, `transcript_path`, `workspace`),
//! cost and everything else are never named, so they're dropped unread.
//!
//! It never writes to the Claude Code folder, and it never fails loudly: on any
//! error it prints nothing and exits 0, so Claude Code's status line just stays blank.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use agentpulse_core::hash::stable_hash;
use agentpulse_core::time::{UnixMs, now_ms};
use serde::{Deserialize, Serialize};

pub use super::settings_edit::TAP_FLAG;

/// Refuse stdin larger than this.
const MAX_INPUT: u64 = 1024 * 1024;
pub const SNAPSHOT_VERSION: u32 = 1;

#[derive(Deserialize)]
struct Input {
    session_id: Option<String>,
    context_window: Option<ContextWindow>,
    rate_limits: Option<RateLimits>,
}

#[derive(Deserialize)]
struct ContextWindow {
    /// Tokens currently in the context window (input incl. cache reads and writes).
    total_input_tokens: Option<u64>,
    context_window_size: Option<u64>,
}

#[derive(Deserialize)]
struct RateLimits {
    five_hour: Option<InWindow>,
    seven_day: Option<InWindow>,
}

#[derive(Deserialize)]
struct InWindow {
    used_percentage: Option<f64>,
    /// Unix epoch seconds.
    resets_at: Option<i64>,
}

/// What the tap stores: numbers and a hashed session id only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub v: u32,
    pub observed_at: UnixMs,
    /// `stable_hash(["claude-session", folder key, session_id])`, the collector's session key.
    pub session_key: Option<String>,
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    pub context: Option<Context>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub used_pct: f64,
    pub resets_at: Option<UnixMs>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub used_tokens: u64,
    pub window_tokens: u64,
}

/// Where snapshots live: `<AgentPulse data folder>/statusline`. Computed from the
/// environment (not Tauri) so the tap and the app agree without starting Tauri.
pub fn tap_dir(app_identifier: &str) -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    }?;
    Some(base.join(app_identifier).join("statusline"))
}

pub fn snapshot_path(dir: &Path, folder_key: &str) -> PathBuf {
    dir.join(format!("{folder_key}.json"))
}

/// The folder key after `TAP_FLAG`, if this process was started as the tap.
pub fn tap_key(args: &[String]) -> Option<&str> {
    let i = args.iter().position(|a| a == TAP_FLAG)?;
    args.get(i + 1).map(String::as_str)
}

/// Run the tap. Always returns exit code 0.
pub fn run(folder_key: &str, dir: &Path, stdin: impl Read, mut stdout: impl Write) -> i32 {
    if let Some(snapshot) = capture(folder_key, stdin, now_ms())
        && write_snapshot(dir, folder_key, &snapshot).is_ok()
    {
        let _ = write!(stdout, "{}", status_text(&snapshot));
    }
    0
}

/// Parse stdin into a snapshot. `None` for a bad key or unparseable input.
pub fn capture(folder_key: &str, stdin: impl Read, now: UnixMs) -> Option<Snapshot> {
    if !is_key(folder_key) {
        return None;
    }
    let mut buf = Vec::new();
    stdin.take(MAX_INPUT + 1).read_to_end(&mut buf).ok()?;
    if buf.len() as u64 > MAX_INPUT {
        return None;
    }
    let input: Input = serde_json::from_slice(&buf).ok()?;
    let window = |w: Option<InWindow>| {
        let w = w?;
        let pct = w.used_percentage?;
        (pct.is_finite() && (0.0..=1000.0).contains(&pct))
            .then(|| Window { used_pct: pct, resets_at: w.resets_at.map(|s| s.saturating_mul(1000)) })
    };
    let (five_hour, seven_day) = match input.rate_limits {
        Some(r) => (window(r.five_hour), window(r.seven_day)),
        None => (None, None),
    };
    let context = input.context_window.and_then(|c| {
        Some(Context { used_tokens: c.total_input_tokens?, window_tokens: c.context_window_size.filter(|w| *w > 0)? })
    });
    Some(Snapshot {
        v: SNAPSHOT_VERSION,
        observed_at: now,
        session_key: input
            .session_id
            .filter(|s| !s.is_empty())
            .map(|s| stable_hash(&["claude-session", folder_key, &s])),
        five_hour,
        seven_day,
        context,
    })
}

/// Overwrite this folder's snapshot (temp file + rename, so readers never see half a file).
pub fn write_snapshot(dir: &Path, folder_key: &str, s: &Snapshot) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = snapshot_path(dir, folder_key);
    let tmp = dir.join(format!("{folder_key}.json.tmp"));
    let json = serde_json::to_vec(s).map_err(|_| std::io::Error::other("serialize"))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)
}

pub fn read_snapshot(dir: &Path, folder_key: &str) -> Option<Snapshot> {
    let bytes = std::fs::read(snapshot_path(dir, folder_key)).ok()?;
    let s: Snapshot = serde_json::from_slice(&bytes).ok()?;
    (s.v == SNAPSHOT_VERSION).then_some(s)
}

/// e.g. `5h 23% · 7d 41% · ctx 8%`. Empty until Claude Code has sent any numbers.
pub fn status_text(s: &Snapshot) -> String {
    let mut parts = Vec::new();
    if let Some(w) = s.five_hour {
        parts.push(format!("5h {:.0}%", w.used_pct));
    }
    if let Some(w) = s.seven_day {
        parts.push(format!("7d {:.0}%", w.used_pct));
    }
    if let Some(c) = s.context {
        parts.push(format!("ctx {:.0}%", c.used_tokens as f64 * 100.0 / c.window_tokens as f64));
    }
    parts.join(" · ")
}

/// The status line command that runs AgentPulse (`exe`) as the tap for one folder.
///
/// Claude Code runs it through Git Bash, or PowerShell when Git Bash is absent, so the
/// path uses forward slashes and `~` for the home folder (documented to work in both).
/// `None` if the path would still contain a space, which neither shell can run
/// unquoted the same way.
pub fn command_for(exe: &Path, user_home: &Path, folder_key: &str) -> Option<String> {
    let slashes = |p: &Path| p.to_string_lossy().replace('\\', "/");
    let path = match exe.strip_prefix(user_home) {
        Ok(rest) => format!("~/{}", slashes(rest)),
        Err(_) => slashes(exe),
    };
    (!path.contains(' ') && is_key(folder_key)).then(|| format!("{path} {TAP_FLAG} {folder_key}"))
}

/// Folder keys are hex hashes; anything else is rejected (no path tricks).
fn is_key(k: &str) -> bool {
    (16..=128).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_hexdigit())
}
