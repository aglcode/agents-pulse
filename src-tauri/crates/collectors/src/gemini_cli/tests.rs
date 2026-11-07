#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use agentpulse_core::batch::{Availability, CursorState, NormalizedBatch};
use agentpulse_core::collector::{Collector, Ctx, Detection, Trigger};
use agentpulse_core::hash::stable_hash;
use agentpulse_core::identity::IdentityKind;
use agentpulse_core::metric::{SourceKind, UnavailableReason};
use agentpulse_core::time::parse_rfc3339_ms;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::testkit::{LogBuilder, Tokens, accounts_json, forbidden_files, settings_json, write_session, write_subagent};
use super::{GeminiCliCollector, TelemetryState};

const T: &str = "2026-11-04T01:00:00.000Z";
const T2: &str = "2026-11-04T01:05:00.000Z";
const T3: &str = "2026-11-04T01:10:00.000Z";
const SENTINEL: &str = "SENTINEL-g3mini-PRIVATE-42";
const PRO: &str = "gemini-2.5-pro";

/// A fake `~/.gemini` signed in with a Google login by default.
struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "agentpulse-gemini-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("tmp")).unwrap();
        std::fs::write(p.join("settings.json"), settings_json("oauth-personal", None)).unwrap();
        std::fs::write(p.join("google_accounts.json"), accounts_json("Me@Example.com")).unwrap();
        Self(p)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tok(input: u64, cached: u64, output: u64, thoughts: u64) -> Tokens {
    Tokens { input, output, cached, thoughts, tool: 0 }
}

async fn collect(c: &GeminiCliCollector, cursors: &HashMap<String, CursorState>) -> NormalizedBatch {
    let ctx = Ctx { home_dir: None, cursors: cursors.clone(), installs: vec![] };
    let mut v = c.collect(&ctx, &CancellationToken::new()).await.unwrap();
    assert_eq!(v.len(), 1);
    v.remove(0)
}

fn committed(b: &NormalizedBatch) -> HashMap<String, CursorState> {
    b.cursors.iter().map(|c| (c.source_key.clone(), c.state.clone())).collect()
}

fn usage_for<'a>(b: &'a NormalizedBatch, message_id: &str) -> Vec<&'a agentpulse_core::batch::UsageEvent> {
    b.usage.iter().filter(|u| u.dedup_key.ends_with(&format!(":{message_id}"))).collect()
}

#[tokio::test]
async fn replies_map_fields_and_the_last_rewrite_wins() {
    let home = Home::new();
    let mut log = LogBuilder::new("sess-1", "main", SENTINEL);
    log.user("u1", T, "hi")
        .reply("g1", PRO, T, tok(1000, 800, 0, 0))
        .reply("g1", PRO, T, tok(1000, 800, 40, 12))
        .noise(T, "x")
        .reply("g2", PRO, T2, tok(1500, 0, 5, 0));
    write_session(&home.0, "proj", "session-2026-11-04T01-00-sess1", &log);

    let b = collect(&GeminiCliCollector::new(home.0.clone()), &HashMap::new()).await;
    assert_eq!(b.usage.len(), 2, "rewrites collapse to one record per reply; user/info/$set ignored");
    let g1 = usage_for(&b, "g1")[0];
    assert_eq!(g1.tokens.input, Some(200), "prompt minus cached");
    assert_eq!(g1.tokens.cache_read, Some(800));
    assert_eq!(g1.tokens.output, Some(52), "the later line wins; output includes thoughts");
    assert_eq!(g1.tokens.reasoning, Some(12));
    assert_eq!(g1.tokens.cache_write, None);
    assert_eq!(g1.model.as_deref(), Some(PRO));
    assert_eq!(g1.occurred_at, parse_rfc3339_ms(T).unwrap());
    assert_eq!(g1.source, SourceKind::Local);
    let session = stable_hash(&["gemini-session", &b.install_key, "sess-1"]);
    assert_eq!(g1.session_key.as_deref(), Some(session.as_str()));
    assert!(!g1.dedup_key.contains("sess-1"), "session id is hashed in the dedup key");
    assert_eq!(b.sessions.len(), 1);
    assert_eq!(b.sessions[0].started_at, parse_rfc3339_ms(T).unwrap());
    assert_eq!(b.sessions[0].last_activity_at, parse_rfc3339_ms(T2).unwrap());
    assert_eq!(b.sessions[0].model.as_deref(), Some(PRO));
    assert!(b.validate().is_ok());
}

#[tokio::test]
async fn subagent_usage_counts_toward_its_parent_session_without_driving_context() {
    let home = Home::new();
    let mut main = LogBuilder::new("parent-1", "main", SENTINEL);
    main.reply("m1", PRO, T, tok(20_000, 0, 10, 0));
    write_session(&home.0, "proj", "session-a", &main);
    let mut sub = LogBuilder::new("sub-x", "subagent", SENTINEL);
    // Same message id as the parent's on purpose: different sessions never collide.
    sub.reply("m1", "gemini-2.5-flash", T3, tok(900_000, 0, 5, 0));
    write_subagent(&home.0, "proj", "parent-1", "sub-x", &sub);

    let b = collect(&GeminiCliCollector::new(home.0.clone()), &HashMap::new()).await;
    assert_eq!(usage_for(&b, "m1").len(), 2, "subagent replies are real API calls");
    let parent = stable_hash(&["gemini-session", &b.install_key, "parent-1"]);
    assert!(b.usage.iter().all(|u| u.session_key.as_deref() == Some(parent.as_str())));
    assert_eq!(b.sessions.len(), 1);
    assert_eq!(b.sessions[0].model.as_deref(), Some(PRO), "session model ignores subagents");
    assert_eq!(b.contexts.len(), 1);
    assert_eq!(b.contexts[0].used_tokens, 20_000, "latest main-thread reply, not the subagent's");
}

#[tokio::test]
async fn context_total_is_estimated_from_the_model_and_unknown_models_get_none() {
    let home = Home::new();
    let mut a = LogBuilder::new("a", "main", SENTINEL);
    a.reply("1", PRO, T, tok(5_000, 4_000, 1, 0)).reply("2", PRO, T2, tok(7_000, 6_000, 1, 0));
    write_session(&home.0, "p", "session-a", &a);
    let mut b_log = LogBuilder::new("b", "main", SENTINEL);
    b_log.reply("3", "gemini-next-unknown", T, tok(100, 0, 1, 0));
    write_session(&home.0, "p", "session-b", &b_log);

    let b = collect(&GeminiCliCollector::new(home.0.clone()), &HashMap::new()).await;
    let known = b.contexts.iter().find(|c| c.used_tokens == 7_000).expect("latest reply, cached included");
    assert_eq!(known.total_tokens, Some(1_048_576));
    assert_eq!(known.total_source, SourceKind::Estimated, "Gemini CLI doesn't log the window size");
    assert_eq!(known.source, SourceKind::Local);
    let unknown = b.contexts.iter().find(|c| c.used_tokens == 100).unwrap();
    assert_eq!(unknown.total_tokens, None, "never guessed");
    assert_eq!(unknown.total_source, SourceKind::Unavailable);
    assert!(b.validate().is_ok());
}

#[tokio::test]
async fn quota_is_unavailable_because_gemini_cli_does_not_expose_it() {
    let home = Home::new();
    let b = collect(&GeminiCliCollector::new(home.0.clone()), &HashMap::new()).await;
    assert!(b.quotas.is_empty());
    let quota = b.availability.iter().find(|a| a.metric == "quota").unwrap();
    assert_eq!(quota.availability, Availability::Unavailable(UnavailableReason::NotExposedByProvider));
    assert!(b.availability.iter().any(|a| a.metric == "tokens" && matches!(a.availability, Availability::Available { .. })));
}

#[tokio::test]
async fn identity_and_scope_follow_the_auth_type() {
    let home = Home::new();
    let c = GeminiCliCollector::new(home.0.clone());
    let b = collect(&c, &HashMap::new()).await;
    assert_eq!(b.entitlement_scope, "google:gemini_cli");
    assert_eq!(b.identity.kind, IdentityKind::VerifiedEmail);
    assert_eq!(b.identity.display_label.as_deref(), Some("Me@Example.com"));

    for (auth, scope, kind) in [
        ("gemini-api-key", "google:gemini_api", IdentityKind::AuthModeOnly),
        ("vertex-ai", "google:vertex", IdentityKind::AuthModeOnly),
        ("something-new", "google:gemini_cli", IdentityKind::None),
    ] {
        std::fs::write(home.0.join("settings.json"), settings_json(auth, None)).unwrap();
        let c = GeminiCliCollector::new(home.0.clone());
        let b = collect(&c, &HashMap::new()).await;
        assert_eq!(b.entitlement_scope, scope, "{auth}");
        assert_eq!(b.identity.kind, kind, "{auth}: the Google account isn't the API key's or project's account");
    }

    // A Google login with no active account recorded: auth mode only.
    std::fs::write(home.0.join("settings.json"), settings_json("oauth-personal", None)).unwrap();
    std::fs::remove_file(home.0.join("google_accounts.json")).unwrap();
    let b = collect(&GeminiCliCollector::new(home.0.clone()), &HashMap::new()).await;
    assert_eq!(b.identity.kind, IdentityKind::AuthModeOnly);
    assert_eq!(b.entitlement_scope, "google:gemini_cli");

    // The older flat settings format.
    std::fs::write(home.0.join("settings.json"), json!({ "selectedAuthType": "gemini-api-key" }).to_string()).unwrap();
    let b = collect(&GeminiCliCollector::new(home.0.clone()), &HashMap::new()).await;
    assert_eq!(b.entitlement_scope, "google:gemini_api");
}

#[test]
fn telemetry_is_detected_from_the_users_own_settings() {
    let home = Home::new();
    let c = || GeminiCliCollector::new(home.0.clone());
    assert_eq!(c().telemetry(), TelemetryState::Off);
    for (t, want) in [
        (json!({ "enabled": false, "target": "local", "outfile": "x" }), TelemetryState::Off),
        (json!({ "enabled": true, "target": "local", "outfile": format!("C:/{SENTINEL}.log") }), TelemetryState::LocalFile),
        (json!({ "enabled": true, "outfile": "x" }), TelemetryState::LocalFile),
        (json!({ "enabled": true, "target": "gcp" }), TelemetryState::Other),
        (json!({ "enabled": true, "target": "local" }), TelemetryState::Other),
    ] {
        std::fs::write(home.0.join("settings.json"), settings_json("oauth-personal", Some(t.clone()))).unwrap();
        assert_eq!(c().telemetry(), want, "{t}");
    }
}

#[tokio::test]
async fn incremental_reads_start_at_the_cursor_and_keep_the_session_from_the_header() {
    let home = Home::new();
    let mut log = LogBuilder::new("s", "main", SENTINEL);
    log.reply("g1", PRO, T, tok(10, 0, 1, 0));
    let path = write_session(&home.0, "p", "session-s", &log);
    let c = GeminiCliCollector::new(home.0.clone());
    let first = collect(&c, &HashMap::new()).await;
    let cursors = committed(&first);
    let first_seq = first.usage[0].source_seq;

    let again = collect(&c, &cursors).await;
    assert!(again.usage.is_empty() && again.cursors.is_empty(), "unchanged file is not read");

    // g1 rewritten with more output, a new reply, and a partial line being written.
    log.reply("g1", PRO, T, tok(10, 0, 50, 0)).reply("g2", PRO, T2, tok(3, 0, 2, 0));
    let mut text = log.build();
    text.push_str("{\"id\":\"g3\",\"type\":\"gemini\",\"tokens\"");
    std::fs::write(&path, &text).unwrap();
    let next = collect(&c, &cursors).await;
    let g1 = usage_for(&next, "g1")[0];
    assert_eq!(g1.tokens.output, Some(50));
    assert!(g1.source_seq > first_seq, "a later line revises the stored record");
    assert_eq!(g1.dedup_key, first.usage[0].dedup_key, "same session from the header, read separately");
    assert_eq!(usage_for(&next, "g2").len(), 1);
    assert!(usage_for(&next, "g3").is_empty());
    assert_eq!(next.cursors[0].state.byte_offset as usize, log.build().len(), "partial line not consumed");
}

#[tokio::test]
async fn a_rewritten_file_is_read_again_from_the_start() {
    let home = Home::new();
    let mut log = LogBuilder::new("s", "main", SENTINEL);
    log.reply("g1", PRO, T, tok(1, 0, 1, 0)).reply("g2", PRO, T2, tok(1, 0, 1, 0));
    let path = write_session(&home.0, "p", "session-s", &log);
    let c = GeminiCliCollector::new(home.0.clone());
    let cursors = committed(&collect(&c, &HashMap::new()).await);
    let mut other = LogBuilder::new("s2", "main", SENTINEL);
    other.reply("x1", PRO, T3, tok(1, 0, 1, 0));
    std::fs::write(&path, other.build()).unwrap();
    let b = collect(&c, &cursors).await;
    assert_eq!(b.usage.len(), 1);
    assert_eq!(usage_for(&b, "x1").len(), 1);
}

#[tokio::test]
async fn a_log_without_a_header_still_counts_under_its_own_file() {
    let home = Home::new();
    let mut log = LogBuilder::new("s", "main", SENTINEL);
    log.reply("g1", PRO, T, tok(1, 0, 1, 0));
    let text = log.build();
    let body = text.split_once('\n').unwrap().1; // drop the header
    let path = home.0.join("tmp/p/chats/session-x.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, body).unwrap();
    let b = collect(&GeminiCliCollector::new(home.0.clone()), &HashMap::new()).await;
    assert_eq!(b.usage.len(), 1);
    assert_eq!(b.sessions.len(), 1);
    assert!(b.validate().is_ok());
}

#[tokio::test]
async fn not_installed_without_a_gemini_home() {
    let home = Home::new();
    let c = GeminiCliCollector::new(home.0.join("missing"));
    assert!(matches!(c.detect(&Ctx::default()).await, Detection::NotInstalled));
    assert!(c.triggers().iter().all(|t| !matches!(t, Trigger::WatchPaths(p) if !p.is_empty())));
    let ctx = Ctx::default();
    assert!(c.collect(&ctx, &CancellationToken::new()).await.unwrap().is_empty());

    let c = GeminiCliCollector::new(home.0.clone());
    assert!(matches!(c.detect(&Ctx::default()).await, Detection::Installed(v) if v.len() == 1));
}

#[tokio::test]
async fn privacy_no_content_paths_or_secrets_reach_the_batch() {
    let home = Home::new();
    forbidden_files(&home.0, &format!("proj-{SENTINEL}"), SENTINEL);
    let mut log = LogBuilder::new(&format!("sess-{SENTINEL}"), "main", SENTINEL);
    log.user("u1", T, &format!("prompt {SENTINEL}"))
        .reply_with("g1", PRO, T2, tok(10, 0, 1, 0), SENTINEL)
        .noise(T2, SENTINEL);
    write_session(&home.0, &format!("proj-{SENTINEL}"), "session-1", &log);
    let mut sub = LogBuilder::new("sub", "subagent", SENTINEL);
    sub.reply_with("g2", PRO, T3, tok(1, 0, 1, 0), SENTINEL);
    write_subagent(&home.0, &format!("proj-{SENTINEL}"), &format!("sess-{SENTINEL}"), "sub", &sub);

    let b = collect(&GeminiCliCollector::new(home.0.clone()), &HashMap::new()).await;
    assert_eq!(b.usage.len(), 2, "usage still collected; the stray log outside chats/ is not");
    let dump = format!("{b:?}");
    assert!(!dump.contains(SENTINEL), "content, a path, a session id or a credential leaked into the batch");
    assert!(!dump.contains("chats"), "a path leaked into the batch");
    assert!(!dump.contains(&home.0.to_string_lossy().to_string()), "home path leaked into the batch");
    assert!(!dump.contains("me@example.com"), "the identity key is a secret (only the label shows the email)");
}

/// Collecting must never change anything in the Gemini CLI home.
#[tokio::test]
async fn collecting_never_writes_to_the_gemini_home() {
    let home = Home::new();
    forbidden_files(&home.0, "p", SENTINEL);
    let mut log = LogBuilder::new("s", "main", SENTINEL);
    log.reply_with("g1", PRO, T, tok(1, 0, 1, 0), SENTINEL);
    write_session(&home.0, "p", "session-s", &log);
    write_subagent(&home.0, "p", "s", "sub", &log);

    let before = snapshot(&home.0);
    let c = GeminiCliCollector::new(home.0.clone());
    let first = collect(&c, &HashMap::new()).await;
    let _ = c.detect(&Ctx::default()).await;
    let _ = c.telemetry();
    let _ = c.triggers();
    let _ = collect(&c, &committed(&first)).await;
    assert_eq!(snapshot(&home.0), before, "a file in the Gemini CLI home changed");
}

/// Every file under `root`: (size, mtime, content hash).
fn snapshot(root: &Path) -> BTreeMap<String, (u64, std::time::SystemTime, String)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                stack.push(p);
            } else {
                let m = e.metadata().unwrap();
                let content = String::from_utf8_lossy(&std::fs::read(&p).unwrap()).to_string();
                out.insert(p.to_string_lossy().to_string(), (m.len(), m.modified().unwrap(), stable_hash(&[&content])));
            }
        }
    }
    out
}

/// Manual check against the real `~/.gemini` (read-only). Prints counts, models and
/// token sums only. Run with:
/// cargo test -p agentpulse-collectors real_gemini_home -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn real_gemini_home_summary() {
    let user_home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap();
    let c = GeminiCliCollector::new(GeminiCliCollector::default_home(&user_home));
    let started = std::time::Instant::now();
    let b = collect(&c, &HashMap::new()).await;
    let sum = |f: fn(&agentpulse_core::usage::TokenUsage) -> Option<u64>| b.usage.iter().filter_map(|u| f(&u.tokens)).sum::<u64>();
    let mut models: Vec<_> = b.usage.iter().filter_map(|u| u.model.clone()).collect();
    models.sort();
    models.dedup();
    println!(
        "elapsed {:?}; scope {}, identity {:?}, telemetry {:?}, files {}, sessions {}, usage records {}, contexts {}, valid {:?}",
        started.elapsed(),
        b.entitlement_scope,
        b.identity.kind,
        c.telemetry(),
        b.cursors.len(),
        b.sessions.len(),
        b.usage.len(),
        b.contexts.len(),
        b.validate()
    );
    println!(
        "tokens: input {} output {} cache_read {} reasoning {}; models {models:?}",
        sum(|t| t.input),
        sum(|t| t.output),
        sum(|t| t.cache_read),
        sum(|t| t.reasoning)
    );
    for ctx in &b.contexts {
        println!("context: used {} total {:?} ({:?})", ctx.used_tokens, ctx.total_tokens, ctx.total_source);
    }
}
