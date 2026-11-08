//! End-to-end Antigravity collector tests: a synthetic `~/.gemini` with conversation
//! databases → collector → engine → SQLite.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use agentpulse_collectors::antigravity::testkit::{
    CLAUDE_OPUS, Conversation, GEMINI_PRO, Tokens, forbidden_files, write_conversation,
};
use agentpulse_collectors::antigravity::AntigravityCollector;
use agentpulse_core::collector::Collector;
use agentpulse_engine::{Engine, EngineConfig};
use agentpulse_store::{open, Reader};

const SENTINEL: &str = "SENTINEL-4ntigrav-e2e-PRIVATE-31";
const T0: i64 = 1_762_000_000;

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

fn start(collector: Arc<AntigravityCollector>, db: &Path) -> (Engine, Reader) {
    let (writer, reader) = open(db).unwrap();
    let c: Arc<dyn Collector> = collector;
    let enabled: HashSet<String> = HashSet::from(["antigravity".to_owned()]);
    let config = EngineConfig { initial_stagger: Duration::ZERO, ..EngineConfig::default() };
    let engine = Engine::start(&tokio::runtime::Handle::current(), vec![c], &enabled, writer, reader.clone(), config);
    (engine, reader)
}

fn tok(input: u64, cache_read: u64, output: u64, thinking: u64) -> Tokens {
    Tokens { input, output, cache_read, cache_write: 0, thinking }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_reaches_the_db_once_with_no_content_ids_or_paths_in_db_or_logs() {
    let logs = captured_logs();
    let dir = Dir::new("antigravity");
    let gemini = dir.0.join("home").join(".gemini");
    forbidden_files(&gemini, "antigravity-ide", SENTINEL);

    let mut conv = Conversation::new(SENTINEL);
    conv.plain_step(T0)
        .call(&format!("r1-{SENTINEL}"), GEMINI_PRO, Some("gemini-pro-default"), T0 + 1, tok(100, 900, 50, 20))
        .call(&format!("r2-{SENTINEL}"), CLAUDE_OPUS, Some("claude-opus-4-6-thinking"), T0 + 2, tok(10, 0, 5, 0))
        .aux_call(&format!("aux-{SENTINEL}"), T0 + 3, tok(7, 0, 1, 0));
    let path = write_conversation(&gemini.join("antigravity-ide"), &format!("conv-{SENTINEL}"), &conv);

    let (engine, reader) = start(Arc::new(AntigravityCollector::new(gemini.clone())), &dir.0.join("db.sqlite"));
    assert_eq!(wait_for(&reader, "SELECT COUNT(*) FROM usage_records", 3).await, 3, "generation records aren't counted again");
    assert_eq!(count(&reader, "SELECT SUM(output_tokens) FROM usage_records"), 56, "thinking is inside output");
    assert_eq!(count(&reader, "SELECT SUM(reasoning_tokens) FROM usage_records"), 20);
    assert_eq!(count(&reader, "SELECT SUM(cache_read_tokens) FROM usage_records"), 900);
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_accounts WHERE kind = 'install_scoped'"), 1);
    assert_eq!(strings(&reader, "SELECT entitlement_scope FROM usage_accounts"), vec!["google:antigravity"]);
    let mut models = strings(&reader, "SELECT DISTINCT m.name FROM usage_records r JOIN models m ON m.id = r.model_id");
    models.sort();
    assert_eq!(models, vec!["claude-opus-4-6-thinking", "gemini-pro-default", "unknown"], "the auxiliary model has no name");
    let reasons = strings(&reader, "SELECT reason FROM metric_availability WHERE metric = 'quota'");
    assert_eq!(reasons.len(), 1);
    assert!(reasons[0].contains("not_exposed_by_provider"), "{reasons:?}");
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM sessions"), 1);
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM context_segments"), 1);
    assert_eq!(strings(&reader, "SELECT total_source FROM context_segments"), vec!["unavailable"]);

    // File watcher: a new call triggers a run, and the whole conversation is re-read
    // without counting the earlier calls twice.
    tokio::time::sleep(Duration::from_millis(50)).await;
    conv.call(&format!("r3-{SENTINEL}"), GEMINI_PRO, Some("gemini-pro-default"), T0 + 60, tok(1, 0, 1, 0));
    conv.write(&path);
    assert_eq!(wait_for(&reader, "SELECT COUNT(*) FROM usage_records", 4).await, 4, "watcher picked up the change");

    engine.refresh(Some("antigravity"));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_records"), 4);
    assert_eq!(count(&reader, "SELECT SUM(record_count) FROM usage_hourly"), 4);
    assert_eq!(count(&reader, "SELECT SUM(output_tokens) FROM usage_hourly"), 57);

    let report = engine.shutdown(Duration::from_secs(2), Duration::from_secs(2)).await;
    assert!(report.writer_closed_cleanly);

    let db = dump_db(&reader);
    assert!(!db.contains(SENTINEL), "content, an id or a path leaked into the database");
    assert!(!db.contains(&dir.0.to_string_lossy().to_string()), "a path leaked into the database");
    let logs = String::from_utf8_lossy(&logs.lock().unwrap()).to_string();
    assert!(logs.contains("antigravity scan done"), "log capture works");
    assert!(!logs.contains(SENTINEL), "content leaked into logs");
    assert!(!logs.contains(&dir.0.to_string_lossy().to_string()), "a path leaked into logs");
}
