//! Incremental, read-only reading of append-only JSONL logs, shared by collectors
//! whose tools write one JSON object per line (Claude Code, Gemini CLI).
//!
//! * A changed file is read **from its cursor only**, never from the start.
//! * A file is identified by a hash of its first line (up to [`HEAD_BYTES`]). If
//!   that changes, or the file shrank, it's read again from the start; the store's
//!   dedup makes replays no-ops.
//! * A last line without a newline is still being written: it's left for the next
//!   run and the cursor stops before it.
//! * A line longer than [`MAX_LINE`] is skipped and counted as bad.
//!
//! Files are opened read-only. What a line contains is decided by the caller's
//! parser; this module never looks inside a line.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::UNIX_EPOCH;

use agentpulse_core::batch::{CursorState, CursorUpdate};
use agentpulse_core::hash::stable_hash;

/// Bytes hashed to identify a file (its first line is usually much longer).
const HEAD_BYTES: usize = 4096;
/// A single line longer than this is skipped as unparseable (defends against corrupt files).
const MAX_LINE: usize = 64 * 1024 * 1024;

/// What a caller's parser made of one line.
pub(crate) enum LineOutcome<T> {
    /// Not a line the caller wants. Not counted.
    Ignored,
    /// Looked relevant but couldn't be parsed (counted, never logged).
    Bad,
    Item(T),
}

/// What one changed file contributed.
#[derive(Debug)]
pub(crate) struct TailRead<T> {
    pub items: Vec<T>,
    /// Byte offset of each entry in `items` (the revision order for its record).
    pub offsets: Vec<u64>,
    pub bad_lines: u32,
}

impl<T> Default for TailRead<T> {
    fn default() -> Self {
        Self { items: Vec::new(), offsets: Vec::new(), bad_lines: 0 }
    }
}

pub(crate) enum TailOutcome<T> {
    Unchanged,
    Read { read: TailRead<T>, cursor: CursorUpdate },
}

/// Read one log relative to its last committed cursor. `head_tag` namespaces the
/// file-identity hash (it must never change for a collector, or every file is
/// re-read once); `parse` gets each complete line without its line ending.
pub(crate) fn read<T>(
    path: &Path,
    source_key: &str,
    head_tag: &str,
    cursors: &HashMap<String, CursorState>,
    parse: impl FnMut(&[u8]) -> LineOutcome<T>,
) -> std::io::Result<TailOutcome<T>> {
    let meta = fs::metadata(path)?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let previous = cursors.get(source_key);
    if previous.is_some_and(|c| c.size == size && c.mtime == mtime) {
        return Ok(TailOutcome::Unchanged);
    }

    let mut file = File::open(path)?;
    let head = head_id(&mut file, head_tag)?;
    let start = match previous {
        Some(c) if c.file_id == head && c.byte_offset <= size => c.byte_offset,
        _ => 0,
    };
    file.seek(SeekFrom::Start(start))?;
    let (read, consumed) = read_lines(BufReader::new(file), start, parse)?;
    let cursor = CursorUpdate {
        source_key: source_key.to_owned(),
        state: CursorState { byte_offset: consumed, file_id: head, size, mtime },
    };
    Ok(TailOutcome::Read { read, cursor })
}

/// The file's first complete line (without its line ending), if it ends within
/// `max` bytes. For tools that write a header line once and never repeat it.
pub(crate) fn first_line(path: &Path, max: usize) -> std::io::Result<Option<Vec<u8>>> {
    let file = File::open(path)?;
    let mut buf = Vec::new();
    let n = BufReader::new(file).take(max as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 || buf.last() != Some(&b'\n') {
        return Ok(None);
    }
    Ok(Some(trim_eol(&buf).to_vec()))
}

/// Read complete lines from `start`. Returns the parsed items and the offset just
/// past the last complete line.
fn read_lines<T>(
    mut reader: impl BufRead,
    start: u64,
    mut parse: impl FnMut(&[u8]) -> LineOutcome<T>,
) -> std::io::Result<(TailRead<T>, u64)> {
    let mut out = TailRead::default();
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
        match parse(line) {
            LineOutcome::Ignored => {}
            LineOutcome::Bad => out.bad_lines += 1,
            LineOutcome::Item(item) => {
                out.items.push(item);
                out.offsets.push(line_start);
            }
        }
    }
    Ok((out, pos))
}

fn head_id(file: &mut File, tag: &str) -> std::io::Result<String> {
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
    Ok(stable_hash(&[tag, &String::from_utf8_lossy(&head[..end])])[..16].to_owned())
}

fn trim_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn first_line_needs_a_line_ending_within_the_limit() {
        let dir = std::env::temp_dir().join(format!("agentpulse-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.jsonl");
        std::fs::write(&p, "{\"a\":1}\r\n{\"b\":2}\n").unwrap();
        assert_eq!(first_line(&p, 64).unwrap().as_deref(), Some(&b"{\"a\":1}"[..]));
        assert_eq!(first_line(&p, 3).unwrap(), None, "too long");
        std::fs::write(&p, "{\"partial\"").unwrap();
        assert_eq!(first_line(&p, 64).unwrap(), None, "still being written");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
