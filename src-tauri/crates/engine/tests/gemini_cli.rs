//! End-to-end Gemini CLI collector tests: a synthetic `~/.gemini` → collector →
//! engine → SQLite.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use agentpulse_collectors::gemini_cli::testkit::{
    accounts_json, forbidden_files, settings_json, write_session, write_subagent, LogBuilder, Tokens,
};
use agentpulse_collectors::gemini_cli::GeminiCliCollector;
use agentpulse_core::collector::Collector;
use agentpulse_engine::{Engine, EngineConfig};
use agentpulse_store::{open, Reader};

const SENTINEL: &str = "SENTINEL-g3mini-e2e-PRIVATE-23";
const T: &str = "2026-11-04T01:00:00.000Z";
const PRO: &str = "gemini-2.5-pro";

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

fn start(collector: Arc<GeminiCliCollector>, db: &Path) -> (Engine, Reader) {
    let (writer, reader) = open(db).unwrap();
    let c: Arc<dyn Collector> = collector;
    let enabled: HashSet<String> = HashSet::from(["gemini-cli".to_owned()]);
    let config = EngineConfig { initial_stagger: Duration::ZERO, ..EngineConfig::default() };
    let engine = Engine::start(&tokio::runtime::Handle::current(), vec![c], &enabled, writer, reader.clone(), config);
    (engine, reader)
}

fn tokens(input: u64, cached: u64, output: u64, thoughts: u64) -> Tokens {
    Tokens { input, output, cached, thoughts, tool: 0 }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_reaches_the_db_once_with_no_content_paths_or_identity_in_db_or_logs() {
    let logs = captured_logs();
    let dir = Dir::new("gemini");
    let home = dir.0.join("home").join(".gemini");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("settings.json"), settings_json("oauth-personal", None)).unwrap();
    std::fs::write(home.join("google_accounts.json"), accounts_json("me@gmail.example")).unwrap();
    let project = format!("proj-{SENTINEL}");
    forbidden_files(&home, &project, SENTINEL);

    let session = format!("sess-{SENTINEL}");
    let mut main = LogBuilder::new(&session, "main", SENTINEL);
    main.user("u1", T, &format!("prompt {SENTINEL}"))
        .reply_with("g1", PRO, T, tokens(1000, 600, 10, 4), SENTINEL)
        .reply_with("g1", PRO, T, tokens(1000, 600, 40, 4), SENTINEL)
        .noise(T, SENTINEL);
    let main_log = write_session(&home, &project, "session-2026-11-04T01-00-abcd", &main);
    let mut sub = LogBuilder::new("sub-1", "subagent", SENTINEL);
    sub.reply_with("s1", "gemini-2.5-flash", T, tokens(300, 0, 7, 0), SENTINEL);
    write_subagent(&home, &project, &session, "sub-1", &sub);

    let collector = Arc::new(GeminiCliCollector::new(home.clone()));
    let (engine, reader) = start(collector, &dir.0.join("db.sqlite"));
    assert_eq!(wait_for(&reader, "SELECT COUNT(*) FROM usage_records", 2).await, 2);
    assert_eq!(count(&reader, "SELECT SUM(output_tokens) FROM usage_records"), 44 + 7, "later rewrite wins; thoughts in output");
    assert_eq!(count(&reader, "SELECT SUM(cache_read_tokens) FROM usage_records"), 600);
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_accounts WHERE kind = 'verified'"), 1);
    assert_eq!(strings(&reader, "SELECT entitlement_scope FROM usage_accounts"), vec!["google:gemini_cli"]);
    let reasons = strings(&reader, "SELECT reason FROM metric_availability WHERE metric = 'quota'");
    assert_eq!(reasons.len(), 1);
    assert!(reasons[0].contains("not_exposed_by_provider"), "{reasons:?}");
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM sessions"), 1, "the subagent counts toward its parent session");
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM context_segments"), 1);
    assert_eq!(strings(&reader, "SELECT total_source FROM context_segments"), vec!["estimated"]);

    // File watcher: a new reply triggers a run.
    main.reply("g2", PRO, "2026-11-04T01:01:00.000Z", tokens(1100, 1000, 1, 0));
    main.write(&main_log);
    assert_eq!(wait_for(&reader, "SELECT COUNT(*) FROM usage_records", 3).await, 3, "watcher picked up the change");

    // A manual refresh re-reads nothing new and never double-counts.
    engine.refresh(Some("gemini-cli"));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_records"), 3);
    assert_eq!(count(&reader, "SELECT SUM(record_count) FROM usage_hourly"), 3);
    assert_eq!(count(&reader, "SELECT SUM(output_tokens) FROM usage_hourly"), 44 + 7 + 1);

    let report = engine.shutdown(Duration::from_secs(2), Duration::from_secs(2)).await;
    assert!(report.writer_closed_cleanly);

    let db = dump_db(&reader);
    assert!(!db.contains(SENTINEL), "content, a path or a session id leaked into the database");
    assert!(!db.contains(&dir.0.to_string_lossy().to_string()), "a path leaked into the database");
    let logs = String::from_utf8_lossy(&logs.lock().unwrap()).to_string();
    assert!(logs.contains("gemini cli scan done"), "log capture works");
    assert!(!logs.contains(SENTINEL), "content leaked into logs");
    assert!(!logs.contains("me@gmail.example"), "identity leaked into logs");
    assert!(!logs.contains(&dir.0.to_string_lossy().to_string()), "a path leaked into logs");
}
