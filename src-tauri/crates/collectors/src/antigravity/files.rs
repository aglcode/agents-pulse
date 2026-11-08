//! Conversation database discovery and reading for Antigravity's data folders.
//!
//! Antigravity keeps one data folder per install under `~/.gemini/`:
//! `antigravity-ide` (Antigravity IDE) and `antigravity` (the standalone app).
//! Only these files are ever opened, read-only, and only into memory:
//! * `<data folder>/conversations/<id>.db`
//! * `<data folder>/conversations/<id>.db-wal`, the database's write-ahead log
//!
//! Nothing else is listed or opened: not the encrypted `conversations/*.pb`, not
//! `-shm` files, not `brain/`, `knowledge/` or `daemon/` (which holds a CSRF
//! token), and not `~/.gemini/antigravity-backup` (a copy no install writes to).
//! SQLite never touches these files: the bytes are opened as an in-memory database,
//! so no `-shm`, journal or copy is created anywhere.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use agentpulse_core::hash::stable_hash;
use agentpulse_core::time::UnixMs;

/// `(folder name under ~/.gemini, install label)`.
pub(crate) const DATA_FOLDERS: &[(&str, &str)] = &[("antigravity-ide", "Antigravity IDE"), ("antigravity", "Antigravity")];
/// Skip a database or log larger than this (the largest seen is a few MB).
const MAX_FILE: u64 = 512 * 1024 * 1024;
/// Re-read attempts when Antigravity changes a file while it's being read.
const ATTEMPTS: usize = 3;

pub(crate) fn conversations_dir(data_folder: &Path) -> PathBuf {
    data_folder.join("conversations")
}

pub(crate) struct DbFile {
    pub path: PathBuf,
    /// The file name without `.db`: the conversation id. Raw; hashed by the caller.
    pub conversation: String,
    /// Hash of install key + file name: the cursor key. Names are never stored in clear.
    pub source_key: String,
}

/// Every conversation database in a data folder, sorted by name.
pub(crate) fn list(data_folder: &Path, install_key: &str) -> std::io::Result<Vec<DbFile>> {
    let dir = conversations_dir(data_folder);
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "db") || !entry.file_type()?.is_file() {
            continue;
        }
        let Some(conversation) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let source_key = stable_hash(&["antigravity-db", install_key, &conversation]);
        out.push(DbFile { path, conversation, source_key });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

pub(crate) fn wal_path(db: &Path) -> PathBuf {
    let mut s = OsString::from(db.as_os_str());
    s.push("-wal");
    PathBuf::from(s)
}

/// Size and modification time of a file, or `None` if it doesn't exist.
type Stamp = Option<(u64, UnixMs)>;

fn stamp(path: &Path) -> std::io::Result<Stamp> {
    match fs::metadata(path) {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            Ok(Some((m.len(), mtime)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Identity of a database's current state (database + log), from metadata only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Version {
    pub id: String,
    pub size: u64,
    pub mtime: UnixMs,
}

fn version(db: Stamp, wal: Stamp) -> Option<Version> {
    let (size, mtime) = db?;
    let (wal_size, wal_mtime) = wal.unwrap_or((0, 0));
    let id = stable_hash(&["antigravity-db-version", &format!("{size}:{mtime}:{wal_size}:{wal_mtime}")]);
    Some(Version { id, size: size.saturating_add(wal_size), mtime: mtime.max(wal_mtime) })
}

/// The database's current version, from metadata only (nothing is opened).
pub(crate) fn current_version(f: &DbFile) -> std::io::Result<Option<Version>> {
    Ok(version(stamp(&f.path)?, stamp(&wal_path(&f.path))?))
}

pub(crate) struct Snapshot {
    pub db: Vec<u8>,
    pub wal: Vec<u8>,
    pub version: Version,
}

pub(crate) enum ReadResult {
    Read(Snapshot),
    /// The file kept changing while being read, disappeared, or is too large; try next run.
    Skipped,
}

/// Read a database and its log into memory. Both are re-read if either changed
/// while being read, so the pair is consistent.
pub(crate) fn read(f: &DbFile) -> std::io::Result<ReadResult> {
    let wal = wal_path(&f.path);
    for _ in 0..ATTEMPTS {
        let (db_before, wal_before) = (stamp(&f.path)?, stamp(&wal)?);
        let Some(v) = version(db_before, wal_before) else {
            return Ok(ReadResult::Skipped);
        };
        if db_before.is_some_and(|(n, _)| n > MAX_FILE) || wal_before.is_some_and(|(n, _)| n > MAX_FILE) {
            return Ok(ReadResult::Skipped);
        }
        let db_bytes = read_file(&f.path)?;
        let wal_bytes = if wal_before.is_some() { read_file(&wal).unwrap_or_default() } else { Vec::new() };
        if stamp(&f.path)? == db_before && stamp(&wal)? == wal_before {
            return Ok(ReadResult::Read(Snapshot { db: db_bytes, wal: wal_bytes, version: v }));
        }
    }
    Ok(ReadResult::Skipped)
}

/// Read-only open; the whole file into memory.
fn read_file(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}
