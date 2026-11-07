//! Session log discovery and incremental reading for one Claude Code config folder.
//!
//! * Logs are `<config dir>/projects/**/*.jsonl`, including subagent transcripts.
//! * Every assistant line carries its own session id, model and timestamp, so a
//!   changed file is read **from its cursor only** ([`crate::jsonl_tail`]).
//!
//! Files are opened read-only and nothing in the folder is ever written.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use agentpulse_core::batch::CursorState;
use agentpulse_core::hash::stable_hash;

use super::parse::{AssistantLine, Line, parse_line};
use crate::jsonl_tail::{self, LineOutcome, TailOutcome};

/// File-identity hash namespace. Never change it: stored cursors depend on it.
const HEAD_TAG: &str = "claude-head";

pub(crate) struct LogFile {
    pub path: PathBuf,
    /// Hash of install key + relative path: the cursor key. Paths are never stored in clear.
    pub source_key: String,
}

pub(crate) fn projects_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("projects")
}

pub(crate) fn has_logs_folder(config_dir: &Path) -> bool {
    projects_dir(config_dir).is_dir()
}

/// All session logs under `<config dir>/projects`, sorted by path.
pub(crate) fn list(config_dir: &Path, install_key: &str) -> std::io::Result<Vec<LogFile>> {
    let root = projects_dir(config_dir);
    let mut out = Vec::new();
    if root.is_dir() {
        walk(&root, &root, install_key, &mut out)?;
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn walk(root: &Path, dir: &Path, install_key: &str, out: &mut Vec<LogFile>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk(root, &path, install_key, out)?;
            continue;
        }
        if !ft.is_file() || path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        out.push(LogFile { source_key: stable_hash(&["claude-log", install_key, &rel]), path });
    }
    Ok(())
}

pub(crate) type ReadOutcome = TailOutcome<AssistantLine>;

/// Read one log relative to its last committed cursor.
pub(crate) fn read(f: &LogFile, cursors: &HashMap<String, CursorState>) -> std::io::Result<ReadOutcome> {
    jsonl_tail::read(&f.path, &f.source_key, HEAD_TAG, cursors, |line| match parse_line(line) {
        Line::Ignored => LineOutcome::Ignored,
        Line::Bad => LineOutcome::Bad,
        Line::Assistant(a) => LineOutcome::Item(*a),
    })
}
