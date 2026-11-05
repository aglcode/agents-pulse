#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use agentpulse_core::batch::NormalizedBatch;
use agentpulse_core::collector::{Collector, Ctx, Detection};
use agentpulse_core::hash::stable_hash;
use agentpulse_core::identity::IdentityKind;
use agentpulse_core::time::parse_rfc3339_ms;
use tokio_util::sync::CancellationToken;

use super::CodexCollector;
use super::parse::{parse_version, window_kind};
use super::testkit::RolloutBuilder;

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "agentpulse-codex-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("sessions")).unwrap();
        Self(p)
    }
    fn rollout(&self, name: &str) -> PathBuf {
        self.0.join("sessions/2026/11/04").join(name)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn collect(
    c: &CodexCollector,
    cursors: &HashMap<String, agentpulse_core::batch::CursorState>,
) -> NormalizedBatch {
    let ctx = Ctx { home_dir: None, cursors: cursors.clone(), installs: vec![] };
    let mut v = c.collect(&ctx, &CancellationToken::new()).await.unwrap();
    assert_eq!(v.len(), 1);
    v.remove(0)
}

fn committed(b: &NormalizedBatch) -> HashMap<String, agentpulse_core::batch::CursorState> {
    b.cursors.iter().map(|c| (c.source_key.clone(), c.state.clone())).collect()
}

const T: &str = "2026-11-04T01:00:00.000Z";
const T2: &str = "2026-11-04T01:05:00.000Z";
const T3: &str = "2026-11-04T01:10:00.000Z";

#[tokio::test]
async fn records_map_to_usage_with_fresh_input_model_and_session() {
    let home = Home::new();
    let mut r = RolloutBuilder::new();
    r.session_meta("sess-1", "0.155.0", T).turn_context("gpt-5-codex", T).record("resp_a", T2, 1000, 800, 50, 20);
    r.write_plain(&home.rollout("rollout-a.jsonl"));

    let b = collect(&CodexCollector::new(home.0.clone()), &HashMap::new()).await;
    assert_eq!(b.usage.len(), 1);
    let u = &b.usage[0];
    assert_eq!(u.dedup_key, "codex:resp:resp_a");
    assert_eq!(u.tokens.input, Some(200), "input excludes cached");
    assert_eq!(u.tokens.cache_read, Some(800));
    assert_eq!(u.tokens.output, Some(50));
    assert_eq!(u.tokens.reasoning, Some(20));
    assert_eq!(u.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(u.occurred_at, parse_rfc3339_ms(T2).unwrap());
    assert_eq!(u.session_key.as_deref(), Some(stable_hash(&["codex-session", "sess-1"]).as_str()));
    assert_eq!(b.sessions.len(), 1);
    assert_eq!(b.detected_version.as_deref(), Some("0.155.0"));
    assert!(b.validate().is_ok());
}

#[tokio::test]
async fn new_versions_never_count_token_count_usage_alongside_records() {
    let home = Home::new();
    let mut r = RolloutBuilder::new();
    r.session_meta("s", "0.158.0-alpha.2", T).record("resp_1", T, 100, 0, 10, 0).token_count(
        T,
        Some((100, 0, 10)),
        Some((100, 0, 10)),
        Some(258_400),
        None,
    );
    r.write_plain(&home.rollout("rollout-b.jsonl"));
    let b = collect(&CodexCollector::new(home.0.clone()), &HashMap::new()).await;
    assert_eq!(b.usage.len(), 1, "only the per-response record counts");
    assert_eq!(b.contexts.len(), 1, "token_count still provides context");
}

#[tokio::test]
async fn old_versions_use_cumulative_deltas_and_ignore_repeats() {
    let home = Home::new();
    let mut r = RolloutBuilder::new();
    r.session_meta("old", "0.140.0-alpha.2", T)
        .token_count(T, Some((100, 40, 10)), Some((100, 40, 10)), Some(200_000), None)
        .token_count(T2, Some((100, 40, 10)), Some((100, 40, 10)), Some(200_000), None) // repeat: no new usage
        .token_count(T3, Some((250, 90, 30)), Some((150, 50, 20)), Some(200_000), None);
    r.write_plain(&home.rollout("rollout-c.jsonl"));
    let b = collect(&CodexCollector::new(home.0.clone()), &HashMap::new()).await;
    assert_eq!(b.usage.len(), 2);
    let second = &b.usage[1];
    assert_eq!(second.tokens.input, Some(100), "(250-100) input minus (90-40) cached");
    assert_eq!(second.tokens.output, Some(20));
}

#[tokio::test]
async fn quota_windows_are_named_by_length_and_limit_and_deduplicated() {
    let home = Home::new();
    let rl = |pct: f64| Some(("codex", "plus", vec![(pct, 300, 1_793_700_000), (94.0, 10_080, 1_794_000_000)]));
    let mut r = RolloutBuilder::new();
    r.session_meta("q", "0.155.0", T)
        .token_count(T, None, None, None, rl(24.0))
        .token_count(T2, None, None, None, rl(24.0)) // unchanged → dropped, except as the final reading
        .token_count(T3, None, None, None, rl(30.0))
        .token_count(T3, None, None, None, Some(("premium", "plus", vec![(5.0, 300, 1_793_700_000)])));
    r.write_plain(&home.rollout("rollout-d.jsonl"));
    let b = collect(&CodexCollector::new(home.0.clone()), &HashMap::new()).await;

    let five_h: Vec<_> = b.quotas.iter().filter(|q| q.window_kind == "5h").map(|q| q.used_pct).collect();
    assert_eq!(five_h, vec![24.0, 30.0]);
    let seven_d: Vec<_> = b.quotas.iter().filter(|q| q.window_kind == "7d").collect();
    assert_eq!(seven_d.len(), 2, "first reading + final reading (extends the segment)");
    assert_eq!(seven_d[0].resets_at, Some(1_794_000_000_000), "seconds converted to ms");
    assert!(b.quotas.iter().any(|q| q.window_kind == "premium:5h"));
    assert!(b.quotas.windows(2).all(|w| w[0].observed_at <= w[1].observed_at), "sorted by time");
    assert!(b.availability.iter().any(|a| a.metric == "quota"));

    // The plan type becomes the (auth-mode-only) account label. No verified identity.
    assert_eq!(b.identity.kind, IdentityKind::AuthModeOnly);
    assert_eq!(b.identity.display_label.as_deref(), Some("ChatGPT Plus"));
}

#[tokio::test]
async fn context_is_latest_input_over_reported_window() {
    let home = Home::new();
    let mut r = RolloutBuilder::new();
    r.session_meta("c", "0.155.0", T).token_count(T, None, Some((10_000, 0, 5)), Some(258_400), None).token_count(
        T2,
        None,
        Some((57_771, 57_216, 32)),
        Some(258_400),
        None,
    );
    r.write_plain(&home.rollout("rollout-e.jsonl"));
    let b = collect(&CodexCollector::new(home.0.clone()), &HashMap::new()).await;
    assert_eq!(b.contexts.len(), 1);
    assert_eq!(b.contexts[0].used_tokens, 57_771);
    assert_eq!(b.contexts[0].total_tokens, Some(258_400));
}

#[tokio::test]
async fn incremental_reads_emit_only_new_complete_lines() {
    let home = Home::new();
    let path = home.rollout("rollout-f.jsonl");
    let mut r = RolloutBuilder::new();
    r.session_meta("inc", "0.155.0", T).turn_context("gpt-5-codex", T).record("r1", T, 10, 0, 1, 0);
    r.write_plain(&path);
    let c = CodexCollector::new(home.0.clone());
    let first = collect(&c, &HashMap::new()).await;
    assert_eq!(first.usage.len(), 1);
    let cursors = committed(&first);

    // Unchanged file: nothing read, nothing emitted.
    let again = collect(&c, &cursors).await;
    assert!(again.usage.is_empty() && again.cursors.is_empty());

    // Append a complete record plus a partial line still being written.
    r.record("r2", T2, 20, 0, 2, 0);
    let mut text = r.build();
    text.push_str("{\"ordinal\":99,\"type\":\"token_usage_rec");
    std::fs::write(&path, &text).unwrap();
    let next = collect(&c, &cursors).await;
    assert_eq!(next.usage.iter().map(|u| u.dedup_key.as_str()).collect::<Vec<_>>(), vec!["codex:resp:r2"]);
    assert_eq!(next.usage[0].model.as_deref(), Some("gpt-5-codex"), "context from earlier lines still applies");
    assert_eq!(next.cursors[0].state.byte_offset as usize, r.build().len(), "partial line not consumed");
}

#[tokio::test]
async fn compressed_rollouts_are_read_and_skipped_while_unchanged() {
    let home = Home::new();
    let mut r = RolloutBuilder::new();
    r.session_meta("z", "0.155.0", T).record("rz", T, 5, 0, 1, 0);
    r.write_zst(&home.rollout("rollout-z.jsonl.zst"));
    // Archived sessions are read too.
    let mut a = RolloutBuilder::new();
    a.session_meta("arch", "0.155.0", T).record("ra", T, 7, 0, 1, 0);
    a.write_zst(&home.0.join("archived_sessions/rollout-arch.jsonl.zst"));

    let c = CodexCollector::new(home.0.clone());
    let first = collect(&c, &HashMap::new()).await;
    let mut keys: Vec<_> = first.usage.iter().map(|u| u.dedup_key.clone()).collect();
    keys.sort();
    assert_eq!(keys, vec!["codex:resp:ra", "codex:resp:rz"]);
    let second = collect(&c, &committed(&first)).await;
    assert!(second.usage.is_empty());
}

#[tokio::test]
async fn not_installed_without_rollout_folders() {
    let dir = std::env::temp_dir().join(format!("agentpulse-codex-missing-{}", std::process::id()));
    let c = CodexCollector::new(dir);
    assert!(matches!(c.detect(&Ctx::default()).await, Detection::NotInstalled));
}

#[tokio::test]
async fn privacy_no_content_reaches_the_batch() {
    const SENTINEL: &str = "SENTINEL-7f3a-PRIVATE";
    let home = Home::new();
    let mut r = RolloutBuilder::new();
    r.session_meta("p", "0.155.0", T)
        .turn_context("gpt-5-codex", T)
        .content_lines(SENTINEL, T)
        .record("rp", T2, 10, 0, 1, 0)
        .token_count(T2, None, Some((10, 0, 1)), Some(1000), Some(("codex", "plus", vec![(1.0, 300, 1)])));
    r.write_plain(&home.rollout("rollout-p.jsonl"));
    let b = collect(&CodexCollector::new(home.0.clone()), &HashMap::new()).await;
    assert_eq!(b.usage.len(), 1, "usage still collected");
    let dump = format!("{b:?}");
    assert!(!dump.contains(SENTINEL), "content leaked into the batch");
    assert!(!dump.contains("projects/"), "cwd leaked into the batch");
    assert!(!dump.contains(&home.0.to_string_lossy().to_string()), "file path leaked into the batch");
}

#[test]
fn version_and_window_helpers() {
    assert_eq!(parse_version("0.158.0-alpha.2"), Some((0, 158, 0)));
    assert_eq!(parse_version("0.160.0"), Some((0, 160, 0)));
    assert_eq!(parse_version("x"), None);
    assert_eq!(window_kind(Some("codex"), 300), "5h");
    assert_eq!(window_kind(None, 10_080), "7d");
    assert_eq!(window_kind(Some("premium"), 300), "premium:5h");
    assert_eq!(window_kind(Some("codex"), 90), "90m");
}

/// Manual check against the real `~/.codex` (read-only). Prints counts only.
/// Run with: cargo test -p agentpulse-collectors real_codex_home -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn real_codex_home_summary() {
    let home = CodexCollector::default_home(&home_dir());
    let c = CodexCollector::new(home);
    let b = collect(&c, &HashMap::new()).await;
    let sum = |f: fn(&agentpulse_core::usage::TokenUsage) -> Option<u64>| {
        b.usage.iter().filter_map(|u| f(&u.tokens)).sum::<u64>()
    };
    println!("files read: {}, sessions: {}, usage records: {}", b.cursors.len(), b.sessions.len(), b.usage.len());
    println!(
        "tokens: input {} cache_read {} output {} reasoning {}",
        sum(|t| t.input),
        sum(|t| t.cache_read),
        sum(|t| t.output),
        sum(|t| t.reasoning)
    );
    let mut latest: HashMap<&str, (i64, f64, Option<i64>)> = HashMap::new();
    for q in &b.quotas {
        latest.insert(q.window_kind.as_str(), (q.observed_at, q.used_pct, q.resets_at));
    }
    for (k, (at, pct, resets)) in latest {
        println!("quota {k}: {pct}% observed_at={at} resets_at={resets:?}");
    }
    println!("label: {:?}, version: {:?}, valid: {:?}", b.identity.display_label, b.detected_version, b.validate());
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(".").to_path_buf())
}
