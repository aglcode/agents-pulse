//! End-to-end Codex collector tests: synthetic rollouts → collector → engine → SQLite.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use agentpulse_collectors::codex::testkit::RolloutBuilder;
use agentpulse_collectors::codex::CodexCollector;
use agentpulse_core::collector::Collector;
use agentpulse_engine::{Engine, EngineConfig};
use agentpulse_store::{open, Reader};

const SENTINEL: &str = "SENTINEL-c0dex-PRIVATE-91";

struct Dir(PathBuf);
impl Dir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("agentpulse-e2e-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Captures every log line from every thread (global subscriber, set once per test binary).
fn captured_logs() -> Arc<Mutex<Vec<u8>>> {
    static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOGS.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || SharedWriter(sink.clone()))
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        buf
    })
    .clone()
}

struct SharedWriter(Arc<Mutex<Vec<u8>>>);
impl Write for SharedWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn count(reader: &Reader, sql: &str) -> i64 {
    let sql = sql.to_owned();
    reader.with_conn_blocking(move |c| Ok(c.query_row(&sql, [], |r| r.get(0))?)).unwrap()
}

/// Every text and number value in every table, concatenated.
fn dump_db(reader: &Reader) -> String {
    reader
        .with_conn_blocking(|c| {
            let tables: Vec<String> = c
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            let mut out = String::new();
            for t in tables {
                let mut stmt = c.prepare(&format!("SELECT * FROM \"{t}\""))?;
                let n = stmt.column_count();
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    for i in 0..n {
                        let v: rusqlite::types::Value = row.get(i)?;
                        out.push_str(&format!("{v:?}|"));
                    }
                    out.push('\n');
                }
            }
            Ok(out)
        })
        .unwrap()
}


fn start(home: &Path, db: &Path) -> (Engine, Reader) {
    let (writer, reader) = open(db).unwrap();
    let codex: Arc<dyn Collector> = Arc::new(CodexCollector::new(home.to_path_buf()));
    let enabled: HashSet<String> = HashSet::from(["codex".to_owned()]);
    let config = EngineConfig { initial_stagger: Duration::ZERO, ..EngineConfig::default() };
    let engine = Engine::start(&tokio::runtime::Handle::current(), vec![codex], &enabled, writer, reader.clone(), config);
    (engine, reader)
}

async fn wait_for(reader: &Reader, sql: &str, at_least: i64) -> i64 {
    for _ in 0..100 {
        let n = count(reader, sql);
        if n >= at_least {
            return n;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    count(reader, sql)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_end_to_end_keeps_content_out_of_db_and_logs_and_follows_file_changes() {
    let logs = captured_logs();
    let dir = Dir::new("codex");
    let home = dir.0.join("codex-home");
    let rollout = home.join("sessions/2026/11/04/rollout-e2e.jsonl");
    let t = "2026-11-04T01:00:00.000Z";
    let mut r = RolloutBuilder::new();
    r.session_meta("e2e-session", "0.155.0", t)
        .turn_context("gpt-5-codex", t)
        .content_lines(SENTINEL, t)
        .record("resp_1", t, 1000, 400, 50, 10)
        .token_count(t, None, Some((1000, 400, 50)), Some(258_400),
            Some(("codex", "plus", vec![(24.0, 300, 4_102_444_800), (60.0, 10_080, 4_102_444_800)])));
    r.write_plain(&rollout);

    let (engine, reader) = start(&home, &dir.0.join("db.sqlite"));
    assert_eq!(wait_for(&reader, "SELECT COUNT(*) FROM usage_records", 1).await, 1);
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM quota_segments"), 2);
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM context_segments"), 1);
    assert_eq!(count(&reader, "SELECT input_tokens FROM usage_records"), 600, "fresh input = 1000 - 400 cached");
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_accounts WHERE kind = 'install_scoped'"), 1);

    // File watcher: appending a record triggers a run without waiting for the 5-minute poll.
    r.record("resp_2", "2026-11-04T01:01:00.000Z", 200, 0, 20, 0);
    r.write_plain(&rollout);
    assert_eq!(wait_for(&reader, "SELECT COUNT(*) FROM usage_records", 2).await, 2, "watcher picked up the change");

    // A manual refresh re-reads nothing new and never double-counts.
    engine.refresh(Some("codex"));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_records"), 2);
    assert_eq!(count(&reader, "SELECT SUM(record_count) FROM usage_hourly"), 2);

    let report = engine.shutdown(Duration::from_secs(2), Duration::from_secs(2)).await;
    assert!(report.writer_closed_cleanly);

    let db = dump_db(&reader);
    assert!(!db.contains(SENTINEL), "content leaked into the database");
    assert!(!db.contains("projects/"), "cwd leaked into the database");
    assert!(!db.contains(&home.to_string_lossy().to_string()), "file path leaked into the database");
    let logs = String::from_utf8_lossy(&logs.lock().unwrap()).to_string();
    assert!(logs.contains("codex scan done"), "log capture works");
    assert!(!logs.contains(SENTINEL), "content leaked into logs");
    assert!(!logs.contains(&home.to_string_lossy().to_string()), "file path leaked into logs");
}
