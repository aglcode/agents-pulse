//! End-to-end Claude Code collector tests: synthetic config folders → collector →
//! engine → SQLite.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use agentpulse_collectors::claude_code::testkit::{settings_json, write_session, LogBuilder, Tokens};
use agentpulse_collectors::claude_code::ClaudeCodeCollector;
use agentpulse_core::collector::Collector;
use agentpulse_engine::{Engine, EngineConfig};
use agentpulse_store::{open, Reader};

const SENTINEL: &str = "SENTINEL-c1aude-e2e-PRIVATE-17";
const T: &str = "2026-11-04T01:00:00.000Z";

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

fn strings(reader: &Reader, sql: &str) -> Vec<String> {
    let sql = sql.to_owned();
    reader
        .with_conn_blocking(move |c| {
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .unwrap()
}

/// Every value in every table, concatenated.
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

fn start(collector: Arc<ClaudeCodeCollector>, db: &Path) -> (Engine, Reader) {
    let (writer, reader) = open(db).unwrap();
    let c: Arc<dyn Collector> = collector;
    let enabled: HashSet<String> = HashSet::from(["claude-code".to_owned()]);
    let config = EngineConfig { initial_stagger: Duration::ZERO, ..EngineConfig::default() };
    let engine = Engine::start(&tokio::runtime::Handle::current(), vec![c], &enabled, writer, reader.clone(), config);
    (engine, reader)
}

fn tokens(input: u64, output: u64, cache_read: u64) -> Tokens {
    Tokens { input, output, cache_read, cache_write: 0 }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_folders_two_accounts_no_content_in_db_or_logs_and_folder_changes_apply() {
    let logs = captured_logs();
    let dir = Dir::new("claude");
    let home = dir.0.join("home");
    let work = home.join(".claude");
    let personal = home.join(".claude-personal");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&personal).unwrap();
    std::fs::write(home.join(".claude.json"), settings_json("uuid-WORK-1", "me@work.example", "org-w", "claude_team", SENTINEL)).unwrap();
    std::fs::write(personal.join(".claude.json"), settings_json("uuid-HOME-2", "me@home.example", "org-h", "claude_pro", SENTINEL)).unwrap();

    let mut w = LogBuilder::new();
    w.content_lines("ws", SENTINEL, T)
        .assistant("ws", "msg_w1", "claude-opus-4-5", T, tokens(10, 5, 1000))
        .assistant("ws", "msg_w1", "claude-opus-4-5", T, tokens(10, 50, 1000));
    let work_log = write_session(&work, &format!("C--src-{SENTINEL}"), "ws", &w);
    let mut h = LogBuilder::new();
    h.assistant("hs", "msg_h1", "claude-sonnet-4-5", T, tokens(3, 7, 200));
    write_session(&personal, "p", "hs", &h);

    // Start with the default folder only; the personal folder is added later.
    let collector = Arc::new(ClaudeCodeCollector::new(home.clone(), None, vec![]));
    let (engine, reader) = start(collector.clone(), &dir.0.join("db.sqlite"));
    // msg_w1 plus the content-bearing fixture message (its usage counts; its content never does).
    assert_eq!(wait_for(&reader, "SELECT COUNT(*) FROM usage_records", 2).await, 2);
    assert_eq!(count(&reader, "SELECT output_tokens FROM usage_records WHERE dedup_key = 'claude:msg_w1'"), 50, "fullest line wins");
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_accounts"), 1);

    // Adding a folder + restarting the worker picks it up as a separate account.
    collector.set_added_folders(vec![personal.clone()]);
    assert!(engine.restart("claude-code").await.unwrap());
    assert_eq!(wait_for(&reader, "SELECT COUNT(*) FROM usage_records", 3).await, 3);
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_accounts WHERE kind = 'verified'"), 2);
    assert_eq!(count(&reader, "SELECT COUNT(DISTINCT usage_account_id) FROM usage_records"), 2, "usage never shared");
    let mut labels = strings(&reader, "SELECT display_label FROM usage_accounts ORDER BY display_label");
    labels.sort();
    assert_eq!(labels, vec!["Claude Code · me@home.example (Pro)", "Claude Code · me@work.example (Team)"]);
    let reasons = strings(&reader, "SELECT reason FROM metric_availability WHERE metric = 'quota'");
    assert_eq!(reasons.len(), 2);
    assert!(reasons.iter().all(|r| r.contains("requires_tool_settings_change")));
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM context_segments"), 2);

    // File watcher: a new message in the work log triggers a run.
    w.assistant("ws", "msg_w2", "claude-opus-4-5", "2026-11-04T01:01:00.000Z", tokens(1, 1, 1100));
    w.write(&work_log);
    assert_eq!(wait_for(&reader, "SELECT COUNT(*) FROM usage_records", 4).await, 4, "watcher picked up the change");

    // A manual refresh re-reads nothing new and never double-counts.
    engine.refresh(Some("claude-code"));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_records"), 4);
    assert_eq!(count(&reader, "SELECT SUM(record_count) FROM usage_hourly"), 4);
    assert_eq!(count(&reader, "SELECT SUM(output_tokens) FROM usage_hourly"), 59, "50 + 1 + 7 + 1");

    let report = engine.shutdown(Duration::from_secs(2), Duration::from_secs(2)).await;
    assert!(report.writer_closed_cleanly);

    let db = dump_db(&reader);
    assert!(!db.contains(SENTINEL), "content leaked into the database");
    assert!(!db.contains("uuid-WORK-1") && !db.contains("uuid-HOME-2"), "account id stored in clear");
    assert!(!db.contains(&home.to_string_lossy().to_string()), "a path leaked into the database");
    let logs = String::from_utf8_lossy(&logs.lock().unwrap()).to_string();
    assert!(logs.contains("claude code scan done"), "log capture works");
    assert!(!logs.contains(SENTINEL), "content leaked into logs");
    assert!(!logs.contains("me@work.example") && !logs.contains("uuid-WORK-1"), "identity leaked into logs");
    assert!(!logs.contains(&home.to_string_lossy().to_string()), "a path leaked into logs");
}
