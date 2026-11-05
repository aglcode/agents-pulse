//! Store integration tests against real SQLite files (architecture §11, M1 exit criteria).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use agentpulse_core::batch::{
    Availability, AvailabilityUpdate, ContextObservation, CursorState, CursorUpdate, NormalizedBatch,
    QuotaObservation, ResolvedBatch, SessionObservation, SessionStatus, UsageEvent,
};
use agentpulse_core::identity::{AccountKind, AccountResolution, IdentityEvidence};
use agentpulse_core::ids::{AgentId, CollectorId, ProviderId};
use agentpulse_core::metric::{SourceKind, Stability, UnavailableReason};
use agentpulse_core::time::HOUR_MS;
use agentpulse_core::usage::TokenUsage;

use crate::{open, AdminOp, AdminResult, FreshnessPolicy, Reader, StoreError, WriterThread};

pub(crate) const INJECT_FAILURE_KEY: &str = "__inject_failure__";

const T0: i64 = 1_793_667_600_000; // 2026-11-03T01:00:00Z, an hour boundary

struct TestDb {
    dir: PathBuf,
    path: PathBuf,
}

impl TestDb {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "agentpulse-store-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        Self { dir, path }
    }

    fn open(&self) -> (WriterThread, Reader) {
        open(&self.path).unwrap()
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn account(key: &str) -> AccountResolution {
    AccountResolution {
        provider: ProviderId::Mock,
        entitlement_scope: "mock:dev".into(),
        account_key: key.into(),
        account_kind: AccountKind::InstallScoped,
        identity: None,
        default_label: format!("Account {key}"),
    }
}

fn batch(install: &str, collected_at: i64) -> NormalizedBatch {
    NormalizedBatch {
        collector_id: CollectorId::new("test"),
        agent: AgentId::Mock,
        entitlement_scope: "mock:dev".into(),
        install_key: install.into(),
        identity: IdentityEvidence::none("test"),
        detected_version: None,
        collected_at,
        sessions: vec![],
        usage: vec![],
        quotas: vec![],
        contexts: vec![],
        availability: vec![],
        cursors: vec![],
    }
}

fn resolved(b: NormalizedBatch, acc: &str) -> ResolvedBatch {
    ResolvedBatch { batch: b, account: account(acc) }
}

fn tokens(input: Option<u64>, output: Option<u64>, cache_read: Option<u64>, cache_write: Option<u64>) -> TokenUsage {
    TokenUsage { input, output, cache_read, cache_write, reasoning: None }
}

fn usage(key: &str, seq: i64, at: i64, model: Option<&str>, t: TokenUsage) -> UsageEvent {
    UsageEvent {
        dedup_key: key.into(),
        source_seq: seq,
        session_key: None,
        model: model.map(str::to_owned),
        occurred_at: at,
        tokens: t,
        source: SourceKind::Local,
        stability: Stability::Undocumented,
    }
}

fn quota(pct: f64, at: i64, resets_at: Option<i64>) -> QuotaObservation {
    QuotaObservation {
        window_kind: "5h".into(),
        used_pct: pct,
        resets_at,
        observed_at: at,
        source: SourceKind::Local,
        stability: Stability::Undocumented,
    }
}

type HourlyRow = (i64, String, i64, i64, [i64; 5], i64);

fn hourly(reader: &Reader) -> Vec<HourlyRow> {
    reader
        .with_conn_blocking(|c| {
            let mut s = c.prepare(
                "SELECT usage_account_id, agent_id, model_id, hour_utc, input_tokens, output_tokens,
                        cache_read_tokens, cache_write_tokens, reasoning_tokens, record_count
                 FROM usage_hourly ORDER BY 1, 2, 3, 4",
            )?;
            let rows = s
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        [r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?],
                        r.get(9)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .unwrap()
}

fn count(reader: &Reader, sql: &str) -> i64 {
    let sql = sql.to_owned();
    reader.with_conn_blocking(move |c| Ok(c.query_row(&sql, [], |r| r.get(0))?)).unwrap()
}

/// Incremental rollups must equal a full recompute from raw records.
async fn assert_rollups_match_rebuild(w: &WriterThread, reader: &Reader) {
    let incremental = hourly(reader);
    let r = w.handle().admin(AdminOp::RebuildRollups).await.unwrap();
    assert!(matches!(r, AdminResult::RollupsRebuilt { .. }));
    assert_eq!(incremental, hourly(reader), "incremental rollups diverged from rebuild");
}

// ---------------------------------------------------------------- schema

#[tokio::test]
async fn migrations_apply_wal_mode_and_reopen_is_idempotent() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let mode: String = reader.with_conn_blocking(|c| Ok(c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?)).unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    assert_eq!(count(&reader, "PRAGMA user_version"), crate::SCHEMA_VERSION);
    assert!(count(&reader, "SELECT COUNT(*) FROM agents") >= 5);
    w.shutdown(Duration::from_secs(2)).await.unwrap();
    drop(reader);

    let (w2, reader2) = db.open();
    assert_eq!(count(&reader2, "PRAGMA user_version"), crate::SCHEMA_VERSION);
    w2.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn no_primary_key_or_unique_column_is_nullable() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let violations = reader
        .with_conn_blocking(|c| {
            let mut violations = Vec::new();
            let tables: Vec<(String, bool)> = c
                .prepare("SELECT name, sql LIKE '%WITHOUT ROWID%' FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            for (table, without_rowid) in tables {
                // (name, type, notnull, pk)
                let cols: Vec<(String, String, bool, i64)> = c
                    .prepare(&format!("SELECT name, type, \"notnull\", pk FROM pragma_table_info('{table}')"))?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                    .collect::<Result<_, _>>()?;
                let notnull = |name: &str| cols.iter().any(|c| c.0 == name && c.2);
                let pk_cols: Vec<_> = cols.iter().filter(|c| c.3 > 0).collect();
                for col in &pk_cols {
                    // `id INTEGER PRIMARY KEY` on a rowid table is the rowid alias and can never be NULL.
                    let rowid_alias = !without_rowid && pk_cols.len() == 1 && col.1.eq_ignore_ascii_case("INTEGER");
                    if !rowid_alias && !col.2 {
                        violations.push(format!("{table}.{} (primary key)", col.0));
                    }
                }
                let indexes: Vec<(String, bool)> = c
                    .prepare(&format!("SELECT name, \"unique\" FROM pragma_index_list('{table}')"))?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                for (index, unique) in indexes.into_iter().filter(|i| i.1) {
                    let _ = unique;
                    let key_cols: Vec<String> = c
                        .prepare(&format!("SELECT name FROM pragma_index_xinfo('{index}') WHERE key = 1 AND cid >= 0"))?
                        .query_map([], |r| r.get(0))?
                        .collect::<Result<_, _>>()?;
                    for col in key_cols {
                        let rowid_alias = !without_rowid && pk_cols.len() == 1 && pk_cols[0].0 == col;
                        if !rowid_alias && !notnull(&col) {
                            violations.push(format!("{table}.{col} (unique index {index})"));
                        }
                    }
                }
            }
            Ok(violations)
        })
        .unwrap();
    assert!(violations.is_empty(), "nullable key columns: {violations:?}");
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

// ---------------------------------------------------------------- dedup and rollups

#[tokio::test]
async fn replaying_the_same_batch_twice_never_double_counts() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let mut b = batch("i1", T0);
    b.usage = vec![
        usage("u1", 1, T0 + 10, Some("m1"), tokens(Some(100), Some(10), None, None)),
        usage("u2", 1, T0 + 20, Some("m1"), tokens(Some(50), Some(5), Some(7), None)),
    ];
    let first = w.handle().submit(resolved(b.clone(), "a")).await.unwrap();
    assert_eq!(first.usage_inserted, 2);
    let snapshot = hourly(&reader);
    for _ in 0..2 {
        let again = w.handle().submit(resolved(b.clone(), "a")).await.unwrap();
        assert_eq!((again.usage_inserted, again.usage_revised, again.usage_unchanged), (0, 0, 2));
    }
    assert_eq!(hourly(&reader), snapshot);
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].4, [150, 15, 7, 0, 0]);
    assert_eq!(snapshot[0].5, 2);
    assert_rollups_match_rebuild(&w, &reader).await;
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn newer_revision_replaces_every_token_field_including_decreases_and_null_to_value() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let mut b = batch("i1", T0);
    b.usage = vec![usage("claude:msg_1", 10, T0 + 5, Some("m1"), tokens(Some(100), Some(1), None, Some(40)))];
    w.handle().submit(resolved(b, "a")).await.unwrap();

    // Final streaming line: input decreased, output grew, cache_read went NULL→value, cache_write unchanged.
    let mut b = batch("i1", T0 + 1);
    b.usage = vec![usage("claude:msg_1", 20, T0 + 5, Some("m1"), tokens(Some(90), Some(300), Some(12), Some(40)))];
    let s = w.handle().submit(resolved(b, "a")).await.unwrap();
    assert_eq!(s.usage_revised, 1);

    let rows = hourly(&reader);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].4, [90, 300, 12, 40, 0]);
    assert_eq!(rows[0].5, 1, "a revision must not add a record");
    let stored: (Option<i64>, Option<i64>) = reader
        .with_conn_blocking(|c| {
            Ok(c.query_row("SELECT input_tokens, cache_read_tokens FROM usage_records", [], |r| Ok((r.get(0)?, r.get(1)?)))?)
        })
        .unwrap();
    assert_eq!(stored, (Some(90), Some(12)));
    assert_rollups_match_rebuild(&w, &reader).await;
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn out_of_order_older_revision_is_ignored() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let mut newer = batch("i1", T0);
    newer.usage = vec![usage("k", 3, T0, Some("m1"), tokens(Some(30), Some(3), None, None))];
    w.handle().submit(resolved(newer, "a")).await.unwrap();
    let mut older = batch("i1", T0 + 1);
    older.usage = vec![usage("k", 2, T0, Some("m1"), tokens(Some(999), Some(999), Some(999), Some(999)))];
    let s = w.handle().submit(resolved(older, "a")).await.unwrap();
    assert_eq!(s.usage_unchanged, 1);
    assert_eq!(hourly(&reader)[0].4, [30, 3, 0, 0, 0]);
    assert_rollups_match_rebuild(&w, &reader).await;
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn revision_moving_hour_and_model_moves_the_rollup_bucket() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let mut b = batch("i1", T0);
    b.usage = vec![
        usage("k", 1, T0 + 10, Some("m1"), tokens(Some(10), Some(1), None, None)),
        usage("other", 1, T0 + 20, Some("m1"), tokens(Some(5), None, None, None)),
    ];
    w.handle().submit(resolved(b, "a")).await.unwrap();

    let mut b = batch("i1", T0 + 1);
    b.usage = vec![usage("k", 2, T0 + HOUR_MS + 10, Some("m2"), tokens(Some(11), Some(2), None, None))];
    w.handle().submit(resolved(b, "a")).await.unwrap();

    let rows = hourly(&reader);
    assert_eq!(rows.len(), 2, "{rows:?}");
    let old_bucket = rows.iter().find(|r| r.3 == T0).unwrap();
    let new_bucket = rows.iter().find(|r| r.3 == T0 + HOUR_MS).unwrap();
    assert_eq!((old_bucket.4, old_bucket.5), ([5, 0, 0, 0, 0], 1));
    assert_eq!((new_bucket.4, new_bucket.5), ([11, 2, 0, 0, 0], 1));
    assert_ne!(old_bucket.2, new_bucket.2, "model changed");

    // Moving the last record out of a bucket deletes the empty row (keeps rebuild equality).
    let mut b = batch("i1", T0 + 2);
    b.usage = vec![usage("other", 2, T0 + HOUR_MS + 30, Some("m2"), tokens(Some(5), None, None, None))];
    w.handle().submit(resolved(b, "a")).await.unwrap();
    assert!(hourly(&reader).iter().all(|r| r.3 == T0 + HOUR_MS));
    assert_rollups_match_rebuild(&w, &reader).await;
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn missing_model_uses_the_unknown_sentinel_without_breaking_keys() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let mut b = batch("i1", T0);
    b.usage = vec![
        usage("a1", 1, T0, None, tokens(Some(1), None, None, None)),
        usage("a2", 1, T0 + 1, None, tokens(Some(2), None, None, None)),
    ];
    w.handle().submit(resolved(b, "a")).await.unwrap();
    let rows = hourly(&reader);
    assert_eq!(rows.len(), 1, "both records must land in the same (sentinel model) bucket");
    assert_eq!(rows[0].4[0], 3);
    assert_rollups_match_rebuild(&w, &reader).await;
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

// ---------------------------------------------------------------- atomicity

#[tokio::test]
async fn injected_mid_transaction_failure_rolls_back_everything_including_the_cursor() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let mut b = batch("i1", T0);
    b.usage = vec![
        usage("good", 1, T0, Some("m1"), tokens(Some(10), None, None, None)),
        usage(INJECT_FAILURE_KEY, 1, T0, Some("m1"), tokens(Some(1), None, None, None)),
    ];
    b.quotas = vec![quota(10.0, T0, None)];
    b.cursors = vec![CursorUpdate {
        source_key: "file".into(),
        state: CursorState { byte_offset: 500, file_id: "f".into(), size: 500, mtime: T0 },
    }];
    let err = w.handle().submit(resolved(b.clone(), "a")).await.unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)));
    for table in ["usage_records", "usage_hourly", "quota_segments", "collector_cursors", "usage_accounts", "collector_status"] {
        assert_eq!(count(&reader, &format!("SELECT COUNT(*) FROM {table}")), 0, "{table} not rolled back");
    }
    // Re-applying the batch without the fault succeeds exactly once.
    b.usage.retain(|u| u.dedup_key != INJECT_FAILURE_KEY);
    w.handle().submit(resolved(b, "a")).await.unwrap();
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM usage_records"), 1);
    assert_eq!(count(&reader, "SELECT byte_offset FROM collector_cursors"), 500);
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn unavailable_observations_are_rejected_and_never_written_as_history() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let mut b = batch("i1", T0);
    let mut q = quota(10.0, T0, None);
    q.source = SourceKind::Unavailable;
    b.quotas = vec![q];
    let err = w.handle().submit(resolved(b, "a")).await.unwrap_err();
    assert!(matches!(err, StoreError::InvalidBatch(_)));
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM quota_segments"), 0);
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

// ---------------------------------------------------------------- segments

#[tokio::test]
async fn quota_segments_extend_open_and_drop_late_readings() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let handle = w.handle();
    let submit = |qs: Vec<QuotaObservation>, at: i64| {
        let mut b = batch("i1", at);
        b.quotas = qs;
        handle.submit(resolved(b, "a"))
    };
    let s = submit(vec![quota(10.0, T0, Some(T0 + 5 * HOUR_MS))], T0).await.unwrap();
    assert_eq!(s.quota_segments_opened, 1);
    let s = submit(vec![quota(10.0, T0 + 60_000, Some(T0 + 5 * HOUR_MS))], T0 + 60_000).await.unwrap();
    assert_eq!(s.quota_segments_extended, 1);
    let s = submit(vec![quota(12.5, T0 + 120_000, Some(T0 + 5 * HOUR_MS))], T0 + 120_000).await.unwrap();
    assert_eq!(s.quota_segments_opened, 1);
    // Late reading (older than latest) must not rewrite history.
    let s = submit(vec![quota(99.0, T0 + 90_000, Some(T0 + 5 * HOUR_MS))], T0 + 130_000).await.unwrap();
    assert_eq!(s.late_observations_dropped, 1);
    // Same percentage but a new reset window opens a new segment.
    let s = submit(vec![quota(12.5, T0 + 180_000, Some(T0 + 10 * HOUR_MS))], T0 + 180_000).await.unwrap();
    assert_eq!(s.quota_segments_opened, 1);

    let segs: Vec<(f64, i64, i64)> = reader
        .with_conn_blocking(|c| {
            let mut s = c.prepare("SELECT used_pct, first_observed_at, last_observed_at FROM quota_segments ORDER BY first_observed_at")?;
            let v = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
            Ok(v)
        })
        .unwrap();
    assert_eq!(segs, vec![(10.0, T0, T0 + 60_000), (12.5, T0 + 120_000, T0 + 120_000), (12.5, T0 + 180_000, T0 + 180_000)]);
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn context_segments_keep_history_per_session() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let ctx = |used: u64, at: i64| ContextObservation {
        session_key: "s1".into(),
        used_tokens: used,
        total_tokens: Some(200_000),
        total_source: SourceKind::Estimated,
        observed_at: at,
        source: SourceKind::Local,
        stability: Stability::Undocumented,
    };
    for (used, at) in [(1000, T0), (1000, T0 + 1), (5000, T0 + 2), (9000, T0 + 3)] {
        let mut b = batch("i1", at);
        b.sessions = vec![SessionObservation {
            session_key: "s1".into(),
            model: Some("m1".into()),
            started_at: T0,
            last_activity_at: at,
            ended_at: None,
            status: SessionStatus::Active,
        }];
        b.contexts = vec![ctx(used, at)];
        w.handle().submit(resolved(b, "a")).await.unwrap();
    }
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM context_segments"), 3);
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM sessions"), 1);
    assert_eq!(count(&reader, "SELECT last_activity_at FROM sessions"), T0 + 3);
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

// ---------------------------------------------------------------- attribution and linking

#[tokio::test]
async fn login_switch_closes_binding_and_preserves_historical_attribution() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let mut b = batch("i1", T0);
    b.usage = vec![usage("before", 1, T0, None, tokens(Some(1), None, None, None))];
    let a = w.handle().submit(resolved(b, "acct-a")).await.unwrap().usage_account_id;

    // Same installation now reports a different account at T0 + 1h.
    let mut b = batch("i1", T0 + HOUR_MS);
    b.usage = vec![
        usage("late-old", 1, T0 + 30 * 60_000, None, tokens(Some(2), None, None, None)), // occurred under A
        usage("after", 1, T0 + HOUR_MS + 1, None, tokens(Some(3), None, None, None)),
    ];
    let bacct = w.handle().submit(resolved(b, "acct-b")).await.unwrap().usage_account_id;
    assert_ne!(a, bacct);

    assert_eq!(count(&reader, "SELECT COUNT(*) FROM agent_bindings WHERE unbound_at IS NULL"), 1);
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM agent_bindings"), 2);
    let owner = |key: &str| count(&reader, &format!("SELECT usage_account_id FROM usage_records WHERE dedup_key = '{key}'"));
    assert_eq!(owner("before"), a);
    assert_eq!(owner("late-old"), a, "event that occurred before the switch belongs to the old account");
    assert_eq!(owner("after"), bacct);
    assert_rollups_match_rebuild(&w, &reader).await;
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn link_and_unlink_never_rewrite_records() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let mut ids = vec![];
    for (install, acct) in [("i1", "a"), ("i2", "b")] {
        let mut b = batch(install, T0);
        b.usage = vec![usage(&format!("{acct}-1"), 1, T0, None, tokens(Some(10), None, None, None))];
        ids.push(w.handle().submit(resolved(b, acct)).await.unwrap().usage_account_id);
    }
    let fingerprint = || {
        reader
            .with_conn_blocking(|c| {
                let mut s = c.prepare("SELECT dedup_key, usage_account_id, input_tokens FROM usage_records ORDER BY dedup_key")?;
                let v: Vec<(String, i64, i64)> = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
                Ok(v)
            })
            .unwrap()
    };
    let before = (fingerprint(), hourly(&reader));

    let linked = w.handle().admin(AdminOp::LinkAccounts { usage_account_ids: ids.clone(), label: "Same plan".into() }).await.unwrap();
    assert!(matches!(linked, AdminResult::Linked { .. }));
    assert_eq!((fingerprint(), hourly(&reader)), before);
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM link_memberships WHERE unlinked_at IS NULL"), 2);
    // Double-linking is refused.
    assert!(w.handle().admin(AdminOp::LinkAccounts { usage_account_ids: ids.clone(), label: "x".into() }).await.is_err());

    w.handle().admin(AdminOp::UnlinkAccount { usage_account_id: ids[0] }).await.unwrap();
    assert_eq!((fingerprint(), hourly(&reader)), before);
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM link_memberships WHERE unlinked_at IS NULL"), 0, "group dissolves below 2");
    assert_eq!(count(&reader, "SELECT COUNT(*) FROM link_memberships"), 2, "membership history is kept");
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn user_label_survives_collector_updates() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let id = w.handle().submit(resolved(batch("i1", T0), "a")).await.unwrap().usage_account_id;
    w.handle().admin(AdminOp::RenameAccount { usage_account_id: id, label: "Work Codex".into() }).await.unwrap();
    w.handle().submit(resolved(batch("i1", T0 + 1), "a")).await.unwrap();
    let label: String = reader
        .with_conn_blocking(move |c| Ok(c.query_row("SELECT display_label FROM usage_accounts WHERE id = ?1", [id], |r| r.get(0))?))
        .unwrap();
    assert_eq!(label, "Work Codex");
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

// ---------------------------------------------------------------- shutdown

#[tokio::test]
async fn submit_after_shutdown_returns_writer_closed_and_writes_nothing() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let straggler = w.handle();
    let mut b = batch("i1", T0);
    b.usage = vec![usage("u1", 1, T0, None, tokens(Some(1), None, None, None))];
    straggler.submit(resolved(b, "a")).await.unwrap();

    w.shutdown(Duration::from_secs(2)).await.unwrap();
    assert!(!straggler.is_accepting());

    let mut late = batch("i1", T0 + 1);
    late.usage = vec![usage("u2", 1, T0 + 1, None, tokens(Some(1), None, None, None))];
    let err = straggler.submit(resolved(late, "a")).await.unwrap_err();
    assert!(matches!(err, StoreError::WriterClosed));
    assert!(matches!(
        straggler.admin(AdminOp::RebuildRollups).await.unwrap_err(),
        StoreError::WriterClosed
    ));
    drop(reader);

    // Reopen: only the pre-shutdown record exists, and the WAL was checkpointed.
    let wal = db.path.with_extension("db-wal");
    assert!(std::fs::metadata(&wal).map(|m| m.len() == 0).unwrap_or(true), "WAL should be truncated");
    let (w2, reader2) = db.open();
    assert_eq!(count(&reader2, "SELECT COUNT(*) FROM usage_records"), 1);
    w2.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn sends_racing_shutdown_are_either_applied_whole_or_rejected() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let handle = w.handle();
    let mut tasks = vec![];
    for i in 0..20 {
        let h = handle.clone();
        tasks.push(tokio::spawn(async move {
            let mut b = batch("i1", T0 + i);
            b.usage = vec![usage(&format!("r{i}"), 1, T0 + i, None, tokens(Some(1), None, None, None))];
            b.cursors = vec![CursorUpdate {
                source_key: format!("f{i}"),
                state: CursorState { byte_offset: 1, file_id: "x".into(), size: 1, mtime: 0 },
            }];
            h.submit(resolved(b, "a")).await.is_ok()
        }));
    }
    w.shutdown(Duration::from_secs(5)).await.unwrap();
    let mut accepted = 0;
    for t in tasks {
        if t.await.unwrap() {
            accepted += 1;
        }
    }
    drop(reader);
    let (w2, reader2) = db.open();
    // Every accepted batch is fully present (record + cursor); no rejected batch left anything behind.
    assert_eq!(count(&reader2, "SELECT COUNT(*) FROM usage_records"), accepted);
    assert_eq!(count(&reader2, "SELECT COUNT(*) FROM collector_cursors"), accepted);
    w2.shutdown(Duration::from_secs(2)).await.unwrap();
}

// ---------------------------------------------------------------- queries

#[tokio::test]
async fn overview_reports_sources_and_unavailability_truthfully() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    // Account a: tokens available, quota observed.
    let mut b = batch("i1", T0);
    b.usage = vec![usage("u1", 1, T0 + 1, Some("m1"), tokens(Some(100), Some(20), None, None))];
    b.quotas = vec![quota(42.0, T0, None)];
    b.availability = vec![
        AvailabilityUpdate { metric: "tokens".into(), availability: Availability::Available { source: SourceKind::Local, stability: Stability::Undocumented } },
        AvailabilityUpdate { metric: "quota".into(), availability: Availability::Available { source: SourceKind::Local, stability: Stability::Undocumented } },
    ];
    w.handle().submit(resolved(b, "a")).await.unwrap();
    // Account b: quota not exposed; tokens never declared.
    let mut b = batch("i2", T0);
    b.availability = vec![AvailabilityUpdate {
        metric: "quota".into(),
        availability: Availability::Unavailable(UnavailableReason::NotExposedByProvider),
    }];
    w.handle().submit(resolved(b, "b")).await.unwrap();

    let now = T0 + 60_000;
    let ov = reader
        .with_conn(move |c| crate::overview(c, now, 0, FreshnessPolicy::default()))
        .await
        .unwrap();
    assert_eq!(ov.accounts.len(), 2);
    let a = &ov.accounts[0];
    assert_eq!(a.tokens_today.source(), SourceKind::Local);
    assert_eq!(a.tokens_today.value().unwrap().input, 100);
    assert_eq!(a.quotas.len(), 1);
    assert_eq!(a.quotas[0].used_pct.value(), Some(&42.0));
    assert!(a.quota_unavailable_reason.is_none());

    let b = &ov.accounts[1];
    assert_eq!(b.tokens_today.source(), SourceKind::Unavailable);
    assert_eq!(b.tokens_today.unavailable_reason(), Some(&UnavailableReason::NotYetCollected));
    assert!(b.tokens_today.value().is_none(), "unavailable must not render as 0");
    assert_eq!(b.quota_unavailable_reason, Some(UnavailableReason::NotExposedByProvider));

    // Much later with no new data, the quota value is presented as stale.
    let later = T0 + 3 * HOUR_MS;
    let ov = reader.with_conn(move |c| crate::overview(c, later, 0, FreshnessPolicy::default())).await.unwrap();
    assert!(matches!(ov.accounts[0].quotas[0].used_pct.freshness(), agentpulse_core::metric::Freshness::Stale { .. }));
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn obsolete_and_reset_quota_windows_are_not_presented_as_current() {
    let db = TestDb::new();
    let (w, reader) = db.open();
    let q = |kind: &str, pct: f64, at: i64, resets: i64| QuotaObservation {
        window_kind: kind.into(),
        used_pct: pct,
        resets_at: Some(resets),
        observed_at: at,
        source: SourceKind::Local,
        stability: Stability::Undocumented,
    };
    let avail = AvailabilityUpdate {
        metric: "quota".into(),
        availability: Availability::Available { source: SourceKind::Local, stability: Stability::Undocumented },
    };
    // An old plan had a 30-day window; the newest snapshot only has 5h + 7d.
    let mut b = batch("i1", T0);
    b.quotas = vec![q("30d", 100.0, T0, T0 + 30 * 24 * HOUR_MS)];
    b.availability = vec![avail.clone()];
    w.handle().submit(resolved(b, "a")).await.unwrap();
    let later = T0 + 10 * 24 * HOUR_MS;
    let mut b = batch("i1", later);
    b.quotas = vec![
        q("5h", 97.0, later, later + HOUR_MS),            // resets in 1h
        q("7d", 23.0, later, later + 5 * 24 * HOUR_MS),   // resets in 5 days
        q("premium:5h", 5.0, later - HOUR_MS, later + HOUR_MS), // separate limit, own snapshot
    ];
    b.availability = vec![avail];
    w.handle().submit(resolved(b, "a")).await.unwrap();

    let overview = |now: i64| reader.with_conn_blocking(move |c| crate::overview(c, now, 0, FreshnessPolicy::default())).unwrap();

    let ov = overview(later + 60_000);
    let kinds: Vec<_> = ov.accounts[0].quotas.iter().map(|q| q.window_kind.as_str()).collect();
    assert_eq!(kinds, vec!["5h", "7d", "premium:5h"], "obsolete 30d window hidden; other limits kept");
    assert_eq!(ov.accounts[0].quotas[0].used_pct.value(), Some(&97.0));

    // Two hours later the 5h window has reset: its old 97% must not be shown.
    let ov = overview(later + 2 * HOUR_MS);
    let five_h = ov.accounts[0].quotas.iter().find(|q| q.window_kind == "5h").unwrap();
    assert!(five_h.used_pct.value().is_none());
    assert_eq!(five_h.used_pct.unavailable_reason(), Some(&UnavailableReason::ResetSinceLastReading));
    let seven_d = ov.accounts[0].quotas.iter().find(|q| q.window_kind == "7d").unwrap();
    assert_eq!(seven_d.used_pct.value(), Some(&23.0), "not yet reset: last reading shown (stale-labelled)");
    w.shutdown(Duration::from_secs(2)).await.unwrap();
}
