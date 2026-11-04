//! Engine integration tests: real SQLite, real Tokio, mock collectors.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agentpulse_collectors::mock::{MockBehavior, MockCollector};
use agentpulse_core::batch::{NormalizedBatch, ResolvedBatch};
use agentpulse_core::collector::Collector;
use agentpulse_core::identity::{IdentityEvidence, IdentityKind};
use agentpulse_core::ids::{AgentId, CollectorId};
use agentpulse_core::secret::SecretString;
use agentpulse_engine::resolver::resolve;
use agentpulse_engine::{Engine, EngineConfig, EngineEvent};
use agentpulse_store::{collector_statuses, open, Reader, StoreError};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "agentpulse-engine-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Self(d)
    }
    fn db(&self) -> PathBuf {
        self.0.join("engine.db")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Start the engine with every given collector enabled.
fn start_all(collectors: Vec<Arc<dyn Collector>>, writer: agentpulse_store::WriterThread, reader: Reader, config: EngineConfig) -> Engine {
    let enabled: HashSet<String> = collectors.iter().map(|c| c.id().0).collect();
    Engine::start(&tokio::runtime::Handle::current(), collectors, &enabled, writer, reader, config)
}

fn fast_config() -> EngineConfig {
    EngineConfig {
        initial_stagger: Duration::ZERO,
        backoff_base: Duration::from_millis(40),
        backoff_max: Duration::from_millis(200),
        ..EngineConfig::default()
    }
}

fn count(reader: &Reader, sql: &str) -> i64 {
    let sql = sql.to_owned();
    reader.with_conn_blocking(move |c| Ok(c.query_row(&sql, [], |r| r.get(0))?)).unwrap()
}

fn mock(id: &str, behavior: MockBehavior, every_ms: u64) -> Arc<dyn Collector> {
    Arc::new(MockCollector::new(id, behavior, Duration::from_millis(every_ms)).with_timeout(Duration::from_millis(150)))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn healthy_collector_writes_and_emits_events() {
    let dir = TestDir::new();
    let (writer, reader) = open(&dir.db()).unwrap();
    let engine = start_all(
        vec![mock("mock-healthy", MockBehavior::Healthy, 30)],
        writer,
        reader.clone(),
        fast_config(),
    );
    let mut events = engine.subscribe();
    let ev = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(EngineEvent::DataChanged { collector_id, .. }) = events.recv().await {
                return collector_id;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(ev, "mock-healthy");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(count(&reader, "SELECT COUNT(*) FROM usage_records") >= 2);
    assert!(count(&reader, "SELECT COUNT(*) FROM quota_segments") >= 1);
    let report = engine.shutdown(Duration::from_secs(1), Duration::from_secs(2)).await;
    assert!(report.writer_closed_cleanly);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panicking_and_hanging_collectors_do_not_affect_a_healthy_one() {
    let dir = TestDir::new();
    let (writer, reader) = open(&dir.db()).unwrap();
    let engine = start_all(
        vec![
            mock("mock-panicking", MockBehavior::Panicking, 30),
            mock("mock-hanging", MockBehavior::Hanging, 30),
            mock("mock-healthy", MockBehavior::Healthy, 30),
        ],
        writer,
        reader.clone(),
        fast_config(),
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let healthy_runs = count(&reader, "SELECT COUNT(*) FROM usage_records");
    assert!(healthy_runs >= 10, "healthy collector kept running ({healthy_runs} runs)");

    let statuses = reader.with_conn(collector_statuses).await.unwrap();
    let by_id = |id: &str| statuses.iter().find(|s| s.collector_id == id).cloned().unwrap();
    let p = by_id("mock-panicking");
    assert_eq!(p.state, "failed");
    assert_eq!(p.last_error.as_ref().unwrap().code, agentpulse_core::metric::ErrorCode::Panicked);
    assert!(p.consecutive_failures >= 2, "keeps retrying with backoff");
    let h = by_id("mock-hanging");
    assert_eq!(h.state, "failed");
    assert_eq!(h.last_error.as_ref().unwrap().code, agentpulse_core::metric::ErrorCode::Timeout);
    assert_eq!(by_id("mock-healthy").state, "ok");

    let started = Instant::now();
    let report = engine.shutdown(Duration::from_secs(1), Duration::from_secs(2)).await;
    assert!(report.writer_closed_cleanly);
    assert!(started.elapsed() < Duration::from_secs(2), "shutdown must not wait on a hung collector");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_written_after_shutdown_begins() {
    let dir = TestDir::new();
    let (writer, reader) = open(&dir.db()).unwrap();
    let engine = start_all(
        vec![
            // Ignores cancellation and finishes its batch after shutdown has started.
            Arc::new(MockCollector::new("mock-slow", MockBehavior::Slow(Duration::from_millis(400)), Duration::from_millis(10))
                .with_timeout(Duration::from_secs(5))) as Arc<dyn Collector>,
            mock("mock-healthy", MockBehavior::Healthy, 20),
        ],
        writer,
        reader.clone(),
        fast_config(),
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    let straggler_handle = engine.writer().clone();

    let report = engine.shutdown(Duration::from_millis(50), Duration::from_secs(2)).await;
    assert!(report.writer_closed_cleanly);
    let at_shutdown = count(&reader, "SELECT COUNT(*) FROM usage_records");

    // Give the slow collector time to finish its sleep; it must not be able to write.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let mut b = minimal_batch("mock-slow");
    b.collected_at = 1;
    let account = resolve(&b).unwrap();
    let err = straggler_handle.submit(ResolvedBatch { batch: b, account }).await.unwrap_err();
    assert!(matches!(err, StoreError::WriterClosed));
    drop(reader);

    let (w2, reader2) = open(&dir.db()).unwrap();
    assert_eq!(count(&reader2, "SELECT COUNT(*) FROM usage_records"), at_shutdown);
    w2.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_runs_a_collector_immediately() {
    let dir = TestDir::new();
    let (writer, reader) = open(&dir.db()).unwrap();
    let engine = start_all(
        vec![mock("mock-hourly", MockBehavior::Healthy, 3_600_000)],
        writer,
        reader.clone(),
        fast_config(),
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_records"), 1);
    assert!(engine.refresh(Some("mock-hourly")));
    assert!(!engine.refresh(Some("nope")));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_records"), 2);
    engine.shutdown(Duration::from_secs(1), Duration::from_secs(2)).await;
}

fn minimal_batch(collector: &str) -> NormalizedBatch {
    NormalizedBatch {
        collector_id: CollectorId::new(collector),
        agent: AgentId::Mock,
        entitlement_scope: "mock:dev".into(),
        install_key: "install".into(),
        identity: IdentityEvidence::none("test"),
        detected_version: None,
        collected_at: 0,
        sessions: vec![],
        usage: vec![],
        quotas: vec![],
        contexts: vec![],
        availability: vec![],
        cursors: vec![],
    }
}

fn google_batch(agent: AgentId, scope: &str, install: &str, email: &str) -> NormalizedBatch {
    NormalizedBatch {
        collector_id: CollectorId::new(agent.as_str()),
        agent,
        entitlement_scope: scope.into(),
        install_key: install.into(),
        identity: IdentityEvidence {
            kind: IdentityKind::VerifiedEmail,
            stable_value: Some(SecretString::from(email)),
            org: None,
            display_label: Some(email.into()),
            evidence: "test".into(),
        },
        detected_version: None,
        collected_at: 1_000,
        sessions: vec![],
        usage: vec![],
        quotas: vec![],
        contexts: vec![],
        availability: vec![],
        cursors: vec![],
    }
}

#[tokio::test]
async fn shared_identity_keeps_usage_accounts_separate_and_avoids_duplicates() {
    let dir = TestDir::new();
    let (writer, reader) = open(&dir.db()).unwrap();
    let w = writer.handle();
    let submit = |b: NormalizedBatch| {
        let account = resolve(&b).unwrap();
        let w = w.clone();
        async move { w.submit(ResolvedBatch { batch: b, account }).await.unwrap().usage_account_id }
    };
    let ag = submit(google_batch(AgentId::Antigravity, "google:antigravity", "ag-1", "me@example.com")).await;
    let gc = submit(google_batch(AgentId::GeminiCli, "google:gemini_cli", "gc-1", "me@example.com")).await;
    let gc_again = submit(google_batch(AgentId::GeminiCli, "google:gemini_cli", "gc-2", "ME@example.com")).await;

    assert_ne!(ag, gc, "Antigravity and Gemini CLI quotas stay separate");
    assert_eq!(gc, gc_again, "same scope + same verified identity = one account, no duplicate");
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_accounts"), 2);
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM identities"), 1, "both accounts reference one identity");
    assert_eq!(count(&reader, "SELECT COUNT(DISTINCT identity_id) FROM usage_accounts"), 1);
    // The raw email is never stored as a key, only as the local display label.
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM identities WHERE identity_key LIKE '%@%'"), 0);
    writer.shutdown(Duration::from_secs(2)).await.unwrap();
}

// ---------------------------------------------------------------- enable / disable (opt-in)

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collectors_are_off_until_enabled_and_the_choice_is_persisted() {
    let dir = TestDir::new();
    let (writer, reader) = open(&dir.db()).unwrap();
    let engine = Engine::start(
        &tokio::runtime::Handle::current(),
        vec![mock("mock-a", MockBehavior::Healthy, 30), mock("mock-b", MockBehavior::Healthy, 30)],
        &HashSet::new(),
        writer,
        reader.clone(),
        fast_config(),
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_records"), 0, "nothing runs before Enable");
    assert!(engine.collectors().iter().all(|c| !c.enabled));
    assert!(!engine.refresh(Some("mock-a")), "a disabled collector can't be refreshed");

    assert!(engine.set_enabled("mock-a", true).await.unwrap());
    assert!(!engine.set_enabled("mock-a", true).await.unwrap(), "idempotent");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(count(&reader, "SELECT COUNT(*) FROM usage_records WHERE agent_id = 'mock'") >= 2);
    assert_eq!(Engine::enabled_from_settings(&reader), HashSet::from(["mock-a".to_owned()]));
    assert!(matches!(engine.set_enabled("nope", true).await, Err(agentpulse_engine::EngineError::UnknownCollector(_))));
    engine.shutdown(Duration::from_secs(1), Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabling_stops_collection_and_marks_metrics_unavailable_without_deleting_history() {
    let dir = TestDir::new();
    let (writer, reader) = open(&dir.db()).unwrap();
    let engine = start_all(vec![mock("mock-a", MockBehavior::Healthy, 30)], writer, reader.clone(), fast_config());
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(engine.set_enabled("mock-a", false).await.unwrap());
    tokio::time::sleep(Duration::from_millis(100)).await; // let any in-flight batch land
    let after_disable = count(&reader, "SELECT COUNT(*) FROM usage_records");
    assert!(after_disable >= 2, "history is kept");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_records"), after_disable, "no collection after disable");

    let statuses = reader.with_conn(collector_statuses).await.unwrap();
    assert_eq!(statuses.iter().find(|s| s.collector_id == "mock-a").unwrap().state, "disabled");
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM metric_availability WHERE state = 'available'"), 0);
    assert!(Engine::enabled_from_settings(&reader).is_empty());

    // The overview shows "collector disabled" instead of presenting old numbers as current.
    let now = agentpulse_core::time::now_ms();
    let ov = reader
        .with_conn(move |c| agentpulse_store::overview(c, now, 0, agentpulse_store::FreshnessPolicy::default()))
        .await
        .unwrap();
    let a = &ov.accounts[0];
    assert!(a.tokens_today.value().is_none());
    assert_eq!(
        a.tokens_today.unavailable_reason(),
        Some(&agentpulse_core::metric::UnavailableReason::CollectorDisabled)
    );
    assert!(a.quotas.is_empty());
    assert_eq!(a.quota_unavailable_reason, Some(agentpulse_core::metric::UnavailableReason::CollectorDisabled));
    let models = reader.with_conn(move |c| agentpulse_store::models_today(c, now, 0)).await.unwrap();
    assert!(models.is_empty(), "per-model breakdown is hidden while disabled");

    // Re-enabling resumes and makes metrics available again.
    assert!(engine.set_enabled("mock-a", true).await.unwrap());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(count(&reader, "SELECT COUNT(*) FROM usage_records") > after_disable);
    assert!(count(&reader, "SELECT COUNT(*) FROM metric_availability WHERE state = 'available'") >= 1);
    engine.shutdown(Duration::from_secs(1), Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enable_all_and_disable_all() {
    let dir = TestDir::new();
    let (writer, reader) = open(&dir.db()).unwrap();
    let engine = Engine::start(
        &tokio::runtime::Handle::current(),
        vec![mock("mock-a", MockBehavior::Healthy, 30), mock("mock-b", MockBehavior::Healthy, 30)],
        &HashSet::new(),
        writer,
        reader.clone(),
        fast_config(),
    );
    assert_eq!(engine.set_all_enabled(true).await.unwrap(), 2);
    assert!(engine.collectors().iter().all(|c| c.enabled));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(count(&reader, "SELECT COUNT(DISTINCT usage_account_id) FROM usage_records"), 2);
    assert_eq!(engine.set_all_enabled(false).await.unwrap(), 2);
    assert!(engine.collectors().iter().all(|c| !c.enabled));
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM collector_status WHERE state = 'disabled'"), 2);
    engine.shutdown(Duration::from_secs(1), Duration::from_secs(2)).await;
}
