//! In-memory replay of a SQLite write-ahead log onto a database image.
//!
//! Antigravity keeps its conversation databases in WAL mode, so recent changes can
//! sit in `<db>-wal` until SQLite checkpoints them. Opening the original with SQLite
//! could create `-shm` or journal files in Antigravity's folder, and a copy on disk
//! would put conversation content on disk. Instead both files are read into memory
//! and the log's committed frames are applied here, following the documented file
//! format (<https://www.sqlite.org/fileformat2.html#walformat>):
//!
//! * the log header and every frame are checked against their salts and cumulative
//!   checksums, and replay stops at the first frame that doesn't match;
//! * only frames up to the last commit frame are applied, so a transaction still
//!   being written is ignored, exactly as SQLite itself would ignore it;
//! * the result is switched to rollback-journal mode in its header so SQLite's
//!   in-memory database accepts it.

const WAL_HEADER: usize = 32;
const FRAME_HEADER: usize = 24;
const MAGIC_LE: u32 = 0x377f_0682;
const MAGIC_BE: u32 = 0x377f_0683;
const WAL_VERSION: u32 = 3_007_000;
const DB_HEADER: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WalOutcome {
    /// No log, an empty log, or a log with no committed frames.
    Nothing,
    /// Committed frames applied.
    Applied { frames: usize },
    /// The log header is invalid or doesn't match the database; the database is used as is.
    Ignored,
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// SQLite's WAL checksum over `data` (a multiple of 8 bytes), continuing from `s`.
fn checksum(data: &[u8], big_endian: bool, mut s: (u32, u32)) -> (u32, u32) {
    for chunk in data.as_chunks::<8>().0 {
        let word = |i: usize| {
            let w = [chunk[i], chunk[i + 1], chunk[i + 2], chunk[i + 3]];
            if big_endian { u32::from_be_bytes(w) } else { u32::from_le_bytes(w) }
        };
        s.0 = s.0.wrapping_add(word(0)).wrapping_add(s.1);
        s.1 = s.1.wrapping_add(word(4)).wrapping_add(s.0);
    }
    s
}

/// Page size recorded in a database header (1 means 65536).
fn db_page_size(db: &[u8]) -> Option<usize> {
    if db.len() < DB_HEADER {
        return None;
    }
    match u16::from_be_bytes([db[16], db[17]]) {
        1 => Some(65_536),
        n => Some(usize::from(n)),
    }
}

/// Apply `wal`'s committed frames to `db` in place.
pub(crate) fn apply(db: &mut Vec<u8>, wal: &[u8]) -> WalOutcome {
    if wal.len() < WAL_HEADER {
        return WalOutcome::Nothing;
    }
    let magic = be32(wal, 0);
    if magic != MAGIC_LE && magic != MAGIC_BE {
        return WalOutcome::Ignored;
    }
    let big_endian = magic == MAGIC_BE;
    let page_size = be32(wal, 8) as usize;
    if be32(wal, 4) != WAL_VERSION || !page_size.is_power_of_two() || !(512..=65_536).contains(&page_size) {
        return WalOutcome::Ignored;
    }
    if db_page_size(db).is_some_and(|p| p != page_size) {
        return WalOutcome::Ignored;
    }
    let mut sum = checksum(&wal[..24], big_endian, (0, 0));
    if sum != (be32(wal, 24), be32(wal, 28)) {
        return WalOutcome::Ignored;
    }
    let salts = (be32(wal, 16), be32(wal, 20));

    // Collect valid frames; remember the last commit frame and the database size it sets.
    let mut frames: Vec<(usize, usize)> = Vec::new();
    let mut committed: Option<(usize, usize)> = None;
    let mut at = WAL_HEADER;
    while at + FRAME_HEADER + page_size <= wal.len() {
        let page_no = be32(wal, at) as usize;
        let db_pages = be32(wal, at + 4) as usize;
        if page_no == 0 || (be32(wal, at + 8), be32(wal, at + 12)) != salts {
            break;
        }
        let body = at + FRAME_HEADER;
        sum = checksum(&wal[at..at + 8], big_endian, sum);
        sum = checksum(&wal[body..body + page_size], big_endian, sum);
        if sum != (be32(wal, at + 16), be32(wal, at + 20)) {
            break;
        }
        frames.push((page_no, body));
        if db_pages != 0 {
            committed = Some((frames.len(), db_pages));
        }
        at = body + page_size;
    }
    let Some((count, db_pages)) = committed else {
        return WalOutcome::Nothing;
    };
    let Some(size) = db_pages.checked_mul(page_size) else {
        return WalOutcome::Ignored;
    };
    db.resize(size, 0);
    for &(page_no, body) in &frames[..count] {
        if page_no <= db_pages {
            let start = (page_no - 1) * page_size;
            db[start..start + page_size].copy_from_slice(&wal[body..body + page_size]);
        }
    }
    WalOutcome::Applied { frames: count }
}

/// Mark an image as a rollback-journal database (header bytes 18–19), so SQLite's
/// in-memory database opens it without looking for a log. Returns false if `db`
/// isn't a SQLite database.
pub(crate) fn detach_wal_mode(db: &mut [u8]) -> bool {
    if db.len() < DB_HEADER || &db[..16] != b"SQLite format 3\0" {
        return false;
    }
    if db[18] == 2 {
        db[18] = 1;
    }
    if db[19] == 2 {
        db[19] = 1;
    }
    true
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use rusqlite::{Connection, MAIN_DB};
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "agentpulse-wal-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A WAL-mode database with checkpointed rows plus uncheckpointed changes
    /// (inserts, an update, a delete, and growth past the original size).
    /// Returns (db bytes, wal bytes, the rows SQLite itself sees).
    fn pending_wal_db(dir: &Tmp) -> (Vec<u8>, Vec<u8>, Vec<(i64, String)>) {
        let path = dir.0.join("t.db");
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
             CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);
             INSERT INTO t(v) VALUES ('a'), ('b'), ('c');",
        )
        .unwrap();
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        for i in 0..200 {
            c.execute("INSERT INTO t(v) VALUES (?1)", [format!("{i}-{}", "x".repeat(300))]).unwrap();
        }
        c.execute_batch("UPDATE t SET v = 'B' WHERE id = 2; DELETE FROM t WHERE id = 3;").unwrap();
        let rows = rows(&c);
        // Copy while the connection is open, so the log isn't checkpointed away.
        let db = std::fs::read(&path).unwrap();
        let wal = std::fs::read(dir.0.join("t.db-wal")).unwrap();
        drop(c);
        (db, wal, rows)
    }

    fn rows(c: &Connection) -> Vec<(i64, String)> {
        let mut s = c.prepare("SELECT id, v FROM t ORDER BY id").unwrap();
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<Result<_, _>>().unwrap()
    }

    fn open(mut db: Vec<u8>) -> rusqlite::Result<Connection> {
        assert!(detach_wal_mode(&mut db));
        let mut c = Connection::open_in_memory()?;
        let len = db.len();
        c.deserialize_read_exact(MAIN_DB, &db[..], len, true)?;
        Ok(c)
    }

    #[test]
    fn replay_matches_what_sqlite_sees() {
        let dir = Tmp::new("match");
        let (mut db, wal, want) = pending_wal_db(&dir);
        let before = db.len();
        assert!(matches!(apply(&mut db, &wal), WalOutcome::Applied { frames } if frames > 0));
        assert!(db.len() > before, "the database grew in the log");
        assert_eq!(rows(&open(db).unwrap()), want);
    }

    #[test]
    fn without_replay_the_pending_rows_are_missing() {
        let dir = Tmp::new("without");
        let (db, _, want) = pending_wal_db(&dir);
        assert_eq!(rows(&open(db).unwrap()).len(), 3, "only the checkpointed rows");
        assert!(want.len() > 3);
    }

    #[test]
    fn replay_stops_at_a_corrupt_frame_and_ignores_an_uncommitted_tail() {
        let dir = Tmp::new("corrupt");
        let (db, wal, _) = pending_wal_db(&dir);
        let page = be32(&wal, 8) as usize;
        // Corrupt the 3rd frame's page body: only committed frames before it apply.
        let mut bad = wal.clone();
        let third = WAL_HEADER + 2 * (FRAME_HEADER + page) + FRAME_HEADER + 10;
        bad[third] ^= 0xff;
        let mut img = db.clone();
        match apply(&mut img, &bad) {
            WalOutcome::Applied { frames } => assert!(frames < 3),
            WalOutcome::Nothing => {}
            WalOutcome::Ignored => panic!("header is fine"),
        }
        assert!(open(img).is_ok(), "still a consistent database");

        // A truncated last frame is just not applied.
        let mut img = db.clone();
        let cut = &wal[..wal.len() - 7];
        assert!(matches!(apply(&mut img, cut), WalOutcome::Applied { .. } | WalOutcome::Nothing));
        assert!(open(img).is_ok());
    }

    #[test]
    fn bad_headers_are_ignored_and_never_panic() {
        let dir = Tmp::new("headers");
        let (db, wal, _) = pending_wal_db(&dir);
        let mut img = db.clone();
        assert_eq!(apply(&mut img, &[]), WalOutcome::Nothing);
        assert_eq!(apply(&mut img, &wal[..20]), WalOutcome::Nothing);
        let mut w = wal.clone();
        w[0] = 0;
        assert_eq!(apply(&mut img, &w), WalOutcome::Ignored, "bad magic");
        let mut w = wal.clone();
        w[25] ^= 1;
        assert_eq!(apply(&mut img, &w), WalOutcome::Ignored, "bad header checksum");
        let mut w = wal.clone();
        w[8..12].copy_from_slice(&1024u32.to_be_bytes());
        assert_eq!(apply(&mut img, &w), WalOutcome::Ignored, "page size differs from the database");
        assert_eq!(img, db, "nothing applied");
        // Arbitrary bytes after a valid header.
        let mut w = wal[..WAL_HEADER].to_vec();
        w.extend(std::iter::repeat_n(0xab, 9000));
        assert_eq!(apply(&mut img, &w), WalOutcome::Nothing);
    }

    #[test]
    fn non_sqlite_bytes_are_rejected() {
        assert!(!detach_wal_mode(&mut [0u8; 200]));
        assert!(!detach_wal_mode(&mut [0u8; 10]));
    }
}
