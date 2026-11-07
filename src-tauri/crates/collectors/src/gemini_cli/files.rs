//! Session log discovery and incremental reading for one Gemini CLI home (`~/.gemini`).
//!
//! Only these files are ever opened, read-only:
//! * `tmp/<project>/chats/*.jsonl`: a session's log;
//! * `tmp/<project>/chats/<parent session id>/*.jsonl`: a subagent's log, stored
//!   under the session that started it.
//!
//! Nothing else under `tmp/` is listed or opened: not `logs.json` (prompt history),
//! not `.project_root` (a path), and no deeper folders.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use agentpulse_core::batch::CursorState;
use agentpulse_core::hash::stable_hash;

use super::parse::{Header, ReplyLine, parse_header, parse_line};
use crate::jsonl_tail::{self, TailOutcome};

/// File-identity hash namespace. Never change it: stored cursors depend on it.
const HEAD_TAG: &str = "gemini-head";
/// The header is the first line; give up on one longer than this.
const MAX_HEADER: usize = 1024 * 1024;

pub(crate) struct LogFile {
    pub path: PathBuf,
    /// Hash of install key + path relative to `tmp/`: the cursor key. Paths are never stored in clear.
    pub source_key: String,
    /// For a subagent log, the parent session id (its folder name); raw, hashed by the caller.
    pub parent_session: Option<String>,
}

pub(crate) fn tmp_dir(home: &Path) -> PathBuf {
    home.join("tmp")
}

/// Gemini CLI has been used here: it keeps per-project logs or settings in its home.
pub(crate) fn is_installed(home: &Path) -> bool {
    tmp_dir(home).is_dir() || home.join("settings.json").is_file()
}

/// All session logs, sorted by path.
pub(crate) fn list(home: &Path, install_key: &str) -> std::io::Result<Vec<LogFile>> {
    let root = tmp_dir(home);
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    for project in fs::read_dir(&root)? {
        let project = project?;
        if !project.file_type()?.is_dir() {
            continue;
        }
        let chats = project.path().join("chats");
        if !chats.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&chats)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_file() {
                push_log(&root, entry.path(), None, install_key, &mut out);
            } else if ft.is_dir() {
                let parent = entry.file_name().to_string_lossy().into_owned();
                for sub in fs::read_dir(entry.path())? {
                    let sub = sub?;
                    if sub.file_type()?.is_file() {
                        push_log(&root, sub.path(), Some(parent.clone()), install_key, &mut out);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn push_log(root: &Path, path: PathBuf, parent_session: Option<String>, install_key: &str, out: &mut Vec<LogFile>) {
    if path.extension().is_none_or(|e| e != "jsonl") {
        return;
    }
    let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
    out.push(LogFile { source_key: stable_hash(&["gemini-log", install_key, &rel]), path, parent_session });
}

pub(crate) type ReadOutcome = TailOutcome<ReplyLine>;

/// Read one log relative to its last committed cursor.
pub(crate) fn read(f: &LogFile, cursors: &HashMap<String, CursorState>) -> std::io::Result<ReadOutcome> {
    jsonl_tail::read(&f.path, &f.source_key, HEAD_TAG, cursors, parse_line)
}

/// The log's header (first line). Message lines don't carry the session id, so it's
/// read separately whenever the file is read from its cursor.
pub(crate) fn header(f: &LogFile) -> Option<Header> {
    jsonl_tail::first_line(&f.path, MAX_HEADER).ok().flatten().and_then(|l| parse_header(&l))
}
