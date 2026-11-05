//! Rollout discovery and incremental reading.
//!
//! * Plain `.jsonl` rollouts (sessions in progress) are re-read from the start
//!   when they change, so file context (session id, model, totals) is known, but
//!   events are emitted only past the stored cursor.
//! * Compressed `.jsonl.zst` rollouts are finished: they're skipped while their
//!   size and mtime are unchanged.
//! * A `.jsonl` → `.jsonl.zst` rename looks like a new file. Re-emitting its
//!   events is harmless, because dedup makes replays no-ops.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use agentpulse_core::batch::{CursorState, CursorUpdate};
use agentpulse_core::hash::stable_hash;

use super::parse::{FileScan, scan};

/// Refuse to decompress beyond this (defends against corrupt or hostile files).
const MAX_DECOMPRESSED: u64 = 512 * 1024 * 1024;
const ROLLOUT_DIRS: [&str; 2] = ["sessions", "archived_sessions"];

pub(crate) struct Rollout {
    pub path: PathBuf,
    /// Hashed relative path: the cursor key. Paths are never stored in clear.
    pub source_key: String,
    pub compressed: bool,
}

pub(crate) fn watch_dirs(home: &Path) -> Vec<PathBuf> {
    ROLLOUT_DIRS.iter().map(|d| home.join(d)).collect()
}

pub(crate) fn is_installed(home: &Path) -> bool {
    ROLLOUT_DIRS.iter().any(|d| home.join(d).is_dir())
}

/// All rollout files, oldest path first (paths are date-partitioned).
pub(crate) fn list(home: &Path) -> std::io::Result<Vec<Rollout>> {
    let mut out = Vec::new();
    for dir in ROLLOUT_DIRS {
        let root = home.join(dir);
        if root.is_dir() {
            walk(home, &root, &mut out)?;
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn walk(home: &Path, dir: &Path, out: &mut Vec<Rollout>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk(home, &path, out)?;
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("rollout-") {
            continue;
        }
        let compressed = name.ends_with(".jsonl.zst");
        if !compressed && !name.ends_with(".jsonl") {
            continue;
        }
        let rel = path.strip_prefix(home).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        out.push(Rollout { source_key: stable_hash(&["codex-rollout", &rel]), path, compressed });
    }
    Ok(())
}

pub(crate) enum ReadOutcome {
    Unchanged,
    Scanned { scan: Box<FileScan>, cursor: CursorUpdate },
}

/// Read one rollout relative to its last committed cursor.
pub(crate) fn read(r: &Rollout, cursors: &HashMap<String, CursorState>) -> std::io::Result<ReadOutcome> {
    let meta = fs::metadata(&r.path)?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let previous = cursors.get(&r.source_key);
    if previous.is_some_and(|c| c.size == size && c.mtime == mtime) {
        return Ok(ReadOutcome::Unchanged);
    }

    let bytes = if r.compressed { decompress(&r.path)? } else { fs::read(&r.path)? };
    let head = head_id(&bytes);
    // Resume past the cursor only if it is the same file (same first line) and it
    // hasn't been truncated; otherwise start over (dedup keeps replays harmless).
    let emit_from = match previous {
        Some(c) if c.file_id == head && usize::try_from(c.byte_offset).is_ok_and(|o| o <= bytes.len()) => {
            usize::try_from(c.byte_offset).unwrap_or(0)
        }
        _ => 0,
    };
    let scan = scan(&bytes, emit_from, r.compressed, &r.source_key);
    let cursor = CursorUpdate {
        source_key: r.source_key.clone(),
        state: CursorState {
            byte_offset: u64::try_from(scan.consumed).unwrap_or(u64::MAX),
            file_id: head,
            size,
            mtime,
        },
    };
    Ok(ReadOutcome::Scanned { scan: Box::new(scan), cursor })
}

fn decompress(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let decoder = ruzstd::decoding::StreamingDecoder::new(std::io::BufReader::new(file))
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "zstd frame"))?;
    let mut out = Vec::new();
    let read = decoder.take(MAX_DECOMPRESSED + 1).read_to_end(&mut out)?;
    if read as u64 > MAX_DECOMPRESSED {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "rollout too large"));
    }
    Ok(out)
}

/// Identifies a file by its first line (the session header), hashed.
fn head_id(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == b'\n').unwrap_or(bytes.len()).min(4096);
    let head = String::from_utf8_lossy(&bytes[..end]);
    stable_hash(&["codex-head", &head])[..16].to_owned()
}
