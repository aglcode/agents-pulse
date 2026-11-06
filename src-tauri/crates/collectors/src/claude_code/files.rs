//! Session log discovery and incremental reading for one Claude Code config folder.
//!
//! * Logs are `<config dir>/projects/**/*.jsonl`, including subagent transcripts.
//! * Every assistant line carries its own session id, model and timestamp, so a
//!   changed file is read **from its cursor only**, never from the start.
//! * A file is identified by a hash of its first bytes. If that changes, or the
//!   file shrank, it's read again from the start; dedup makes replays no-ops.
//! * A last line without a newline is still being written: it's left for the
//!   next run and the cursor stops before it.
//!
//! Files are opened read-only and nothing in the folder is ever written.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use agentpulse_core::batch::{CursorState, CursorUpdate};
use agentpulse_core::hash::stable_hash;

use super::parse::{AssistantLine, Line, parse_line};

/// Bytes hashed to identify a file (its first line is usually much longer).
const HEAD_BYTES: usize = 4096;
/// A single line longer than this is skipped as unparseable (defends against corrupt files).
const MAX_LINE: usize = 64 * 1024 * 1024;

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

/// What one changed file contributed.
#[derive(Debug, Default)]
pub(crate) struct FileRead {
    pub lines: Vec<AssistantLine>,
    /// Byte offset of each entry in `lines` (the revision order for its message).
    pub offsets: Vec<u64>,
    pub bad_lines: u32,
}

pub(crate) enum ReadOutcome {
    Unchanged,
    Read { read: FileRead, cursor: CursorUpdate },
}

/// Read one log relative to its last committed cursor.
pub(crate) fn read(f: &LogFile, cursors: &HashMap<String, CursorState>) -> std::io::Result<ReadOutcome> {
    let meta = fs::metadata(&f.path)?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let previous = cursors.get(&f.source_key);
    if previous.is_some_and(|c| c.size == size && c.mtime == mtime) {
        return Ok(ReadOutcome::Unchanged);
    }

    let mut file = File::open(&f.path)?;
    let head = head_id(&mut file)?;
    let start = match previous {
        Some(c) if c.file_id == head && c.byte_offset <= size => c.byte_offset,
        _ => 0,
    };
    file.seek(SeekFrom::Start(start))?;
    let (read, consumed) = read_lines(BufReader::new(file), start)?;
    let cursor = CursorUpdate {
        source_key: f.source_key.clone(),
        state: CursorState { byte_offset: consumed, file_id: head, size, mtime },
    };
    Ok(ReadOutcome::Read { read, cursor })
}

/// Read complete lines from `start`. Returns the parsed lines and the offset just
/// past the last complete line.
fn read_lines(mut reader: impl BufRead, start: u64) -> std::io::Result<(FileRead, u64)> {
    let mut out = FileRead::default();
    let mut pos = start;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.by_ref().take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        if buf.last() != Some(&b'\n') {
            if n > MAX_LINE {
                // Oversized line: skip to its end, then carry on.
                let mut rest = Vec::new();
                let skipped = reader.read_until(b'\n', &mut rest)?;
                if rest.last() != Some(&b'\n') {
                    break; // still being written; retry next run
                }
                pos += (n + skipped) as u64;
                out.bad_lines += 1;
                continue;
            }
            break; // partial last line still being written
        }
        let line_start = pos;
        pos += n as u64;
        let line = trim_eol(&buf);
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match parse_line(line) {
            Line::Ignored => {}
            Line::Bad => out.bad_lines += 1,
            Line::Assistant(a) => {
                out.lines.push(*a);
                out.offsets.push(line_start);
            }
        }
    }
    Ok((out, pos))
}

fn head_id(file: &mut File) -> std::io::Result<String> {
    let mut head = vec![0u8; HEAD_BYTES];
    let mut filled = 0;
    while filled < HEAD_BYTES {
        let n = file.read(&mut head[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    head.truncate(filled);
    let end = head.iter().position(|b| *b == b'\n').unwrap_or(head.len());
    Ok(stable_hash(&["claude-head", &String::from_utf8_lossy(&head[..end])])[..16].to_owned())
}

fn trim_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}
