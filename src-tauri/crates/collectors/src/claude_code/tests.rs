#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use agentpulse_core::batch::{Availability, CursorState, NormalizedBatch};
use agentpulse_core::collector::{Collector, Ctx, Detection};
use agentpulse_core::hash::stable_hash;
use agentpulse_core::identity::IdentityKind;
use agentpulse_core::metric::{SourceKind, UnavailableReason};
use agentpulse_core::time::parse_rfc3339_ms;
use tokio_util::sync::CancellationToken;

use super::testkit::{LogBuilder, Tokens, settings_json, write_session};
use super::{ClaudeCodeCollector, FolderOrigin};

const T: &str = "2026-11-04T01:00:00.000Z";
const T2: &str = "2026-11-04T01:05:00.000Z";
const T3: &str = "2026-11-04T01:10:00.000Z";
const SENTINEL: &str = "SENTINEL-c1aude-PRIVATE-42";

/// A fake user home with `.claude` (default folder) and optionally more folders.
struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "agentpulse-claude-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join(".claude/projects")).unwrap();
        // A supported (Pro) login by default; tests override it to check other plans.
        std::fs::write(p.join(".claude.json"), settings_json("uuid-default", "me@default.example", "org-1", "claude_pro", SENTINEL)).unwrap();
        Self(p)
    }
    fn default_dir(&self) -> PathBuf {
        self.0.join(".claude")
    }
    fn folder(&self, name: &str) -> PathBuf {
        let d = self.0.join(name);
        std::fs::create_dir_all(d.join("projects")).unwrap();
        d
    }
    /// The default folder's settings live in the home, not inside `.claude`.
    fn default_settings(&self, uuid: &str, email: &str, org_type: &str) {
        std::fs::write(self.0.join(".claude.json"), settings_json(uuid, email, "org-1", org_type, SENTINEL)).unwrap();
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tokens(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Tokens {
    Tokens { input, output, cache_read, cache_write }
}

async fn collect_all(c: &ClaudeCodeCollector, cursors: &HashMap<String, CursorState>) -> Vec<NormalizedBatch> {
    let ctx = Ctx { home_dir: None, cursors: cursors.clone(), installs: vec![] };
    c.collect(&ctx, &CancellationToken::new()).await.unwrap()
}

async fn collect_one(c: &ClaudeCodeCollector, cursors: &HashMap<String, CursorState>) -> NormalizedBatch {
    let mut v = collect_all(c, cursors).await;
    assert_eq!(v.len(), 1);
    v.remove(0)
}

fn committed(batches: &[NormalizedBatch]) -> HashMap<String, CursorState> {
    batches.iter().flat_map(|b| &b.cursors).map(|c| (c.source_key.clone(), c.state.clone())).collect()
}

fn collector(home: &Home, added: Vec<PathBuf>) -> ClaudeCodeCollector {
    ClaudeCodeCollector::new(home.0.clone(), None, added)
}

#[tokio::test]
async fn assistant_usage_maps_fields_and_the_last_repeat_wins() {
    let home = Home::new();
    let mut log = LogBuilder::new();
    // One message written three times (one line per content block); output grows.
    log.assistant("s1", "msg_a", "claude-sonnet-4-5", T, tokens(10, 1, 5000, 300))
        .assistant("s1", "msg_a", "claude-sonnet-4-5", T, tokens(10, 1, 5000, 300))
        .assistant("s1", "msg_a", "claude-sonnet-4-5", T, tokens(10, 42, 5000, 300))
        .assistant("s1", "msg_b", "claude-sonnet-4-5", T2, tokens(3, 7, 5300, 0));
    write_session(&home.default_dir(), "C--work-repo", "s1", &log);

    let b = collect_one(&collector(&home, vec![]), &HashMap::new()).await;
    assert_eq!(b.usage.len(), 2, "repeats collapse to one record per message");
    let a = b.usage.iter().find(|u| u.dedup_key == "claude:msg_a").unwrap();
    assert_eq!(a.tokens.input, Some(10), "Anthropic input already excludes cache");
    assert_eq!(a.tokens.output, Some(42), "the fullest (last) line wins");
    assert_eq!(a.tokens.cache_read, Some(5000));
    assert_eq!(a.tokens.cache_write, Some(300));
    assert_eq!(a.tokens.reasoning, None);
    assert_eq!(a.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(a.occurred_at, parse_rfc3339_ms(T).unwrap());
    assert_eq!(a.source, SourceKind::Local);
    let install = b.install_key.clone();
    assert_eq!(a.session_key.as_deref(), Some(stable_hash(&["claude-session", &install, "s1"]).as_str()));
    assert_eq!(b.sessions.len(), 1);
    assert_eq!(b.sessions[0].started_at, parse_rfc3339_ms(T).unwrap());
    assert_eq!(b.sessions[0].last_activity_at, parse_rfc3339_ms(T2).unwrap());
    assert_eq!(b.detected_version.as_deref(), Some("2.1.318"));
    assert!(b.validate().is_ok());
}

#[tokio::test]
async fn synthetic_messages_are_skipped_and_subagents_count_without_driving_context() {
    let home = Home::new();
    let mut log = LogBuilder::new();
    log.assistant("s", "main_1", "claude-opus-4-5", T, tokens(100, 10, 20_000, 0))
        .synthetic("s", T2)
        .assistant_ext("s", "sub_1", "claude-haiku-4-5", T3, tokens(50, 5, 900_000, 0), true);
    write_session(&home.default_dir(), "p", "s", &log);
    let b = collect_one(&collector(&home, vec![]), &HashMap::new()).await;
    let keys: Vec<_> = b.usage.iter().map(|u| u.dedup_key.as_str()).collect();
    assert_eq!(keys, vec!["claude:main_1", "claude:sub_1"], "synthetic message not counted");
    assert_eq!(b.contexts.len(), 1);
    assert_eq!(b.contexts[0].used_tokens, 20_100, "latest main-thread request, not the subagent's");
    assert_eq!(b.sessions[0].model.as_deref(), Some("claude-opus-4-5"), "session model ignores subagents");
}

#[tokio::test]
async fn context_used_is_input_plus_cache_and_the_total_is_labelled_estimated() {
    let home = Home::new();
    let mut log = LogBuilder::new();
    log.assistant("s", "m1", "claude-opus-4-5", T, tokens(5, 1, 40_000, 1_000))
        .assistant("s", "m2", "claude-opus-4-5", T2, tokens(2, 1, 60_000, 500));
    write_session(&home.default_dir(), "p", "s", &log);
    let mut big = LogBuilder::new();
    big.assistant("big", "m3", "claude-sonnet-4-5", T, tokens(10, 1, 250_000, 0));
    write_session(&home.default_dir(), "p", "big", &big);

    let b = collect_one(&collector(&home, vec![]), &HashMap::new()).await;
    let small = b.contexts.iter().find(|c| c.used_tokens == 60_502).expect("latest request of session s");
    assert_eq!(small.total_tokens, Some(200_000));
    assert_eq!(small.total_source, SourceKind::Estimated, "Claude Code doesn't log the window size");
    assert_eq!(small.source, SourceKind::Local);
    let large = b.contexts.iter().find(|c| c.used_tokens == 250_010).unwrap();
    assert_eq!(large.total_tokens, Some(1_000_000), "over 200k implies the 1M window");
}

#[tokio::test]
async fn quota_is_unavailable_because_it_needs_a_settings_change() {
    let home = Home::new();
    home.default_settings("uuid-pro", "me@home.example", "claude_pro");
    let mut log = LogBuilder::new();
    log.assistant("s", "m", "claude-opus-4-5", T, tokens(1, 1, 0, 0));
    write_session(&home.default_dir(), "p", "s", &log);
    let b = collect_one(&collector(&home, vec![]), &HashMap::new()).await;
    assert!(b.quotas.is_empty());
    let quota = b.availability.iter().find(|a| a.metric == "quota").unwrap();
    assert_eq!(quota.availability, Availability::Unavailable(UnavailableReason::RequiresToolSettingsChange));
    assert!(b.availability.iter().any(|a| a.metric == "tokens" && matches!(a.availability, Availability::Available { .. })));
}

#[tokio::test]
async fn identity_comes_from_the_settings_file_with_org_and_plan() {
    let home = Home::new();
    home.default_settings("uuid-work-1", "me@work.example", "claude_max");
    let mut log = LogBuilder::new();
    log.assistant("s", "m", "claude-opus-4-5", T, tokens(1, 1, 0, 0));
    write_session(&home.default_dir(), "p", "s", &log);
    // A stray `.claude/.claude.json` without a login must not hide the real one in the home.
    std::fs::write(home.default_dir().join(".claude.json"), "{\"numStartups\":1}").unwrap();

    let b = collect_one(&collector(&home, vec![]), &HashMap::new()).await;
    assert_eq!(b.entitlement_scope, "anthropic:claude_subscription");
    assert_eq!(b.identity.kind, IdentityKind::VerifiedId);
    assert_eq!(b.identity.org.as_deref(), Some("org-1"));
    assert_eq!(b.identity.display_label.as_deref(), Some("me@work.example (Max)"));
    assert!(!format!("{:?}", b.identity).contains("uuid-work-1"), "account id is a secret");
}

#[tokio::test]
async fn two_folders_signed_in_to_different_accounts_give_two_separate_batches() {
    let home = Home::new();
    home.default_settings("uuid-work", "me@work.example", "claude_max");
    let personal = home.folder(".claude-personal");
    std::fs::write(
        personal.join(".claude.json"),
        settings_json("uuid-personal", "me@home.example", "org-2", "claude_pro", SENTINEL),
    )
    .unwrap();
    let mut work = LogBuilder::new();
    work.assistant("w", "msg_work", "claude-opus-4-5", T, tokens(1, 1, 0, 0));
    write_session(&home.default_dir(), "p", "w", &work);
    let mut mine = LogBuilder::new();
    mine.assistant("h", "msg_home", "claude-sonnet-4-5", T, tokens(2, 2, 0, 0));
    write_session(&personal, "p", "h", &mine);

    let c = collector(&home, vec![personal.clone()]);
    let origins: Vec<_> = c.folders().iter().map(|f| f.origin).collect();
    assert_eq!(origins, vec![FolderOrigin::Default, FolderOrigin::Added]);
    let batches = collect_all(&c, &HashMap::new()).await;
    assert_eq!(batches.len(), 2);
    let (w, h) = (&batches[0], &batches[1]);
    assert_ne!(w.install_key, h.install_key);
    assert_eq!(w.identity.display_label.as_deref(), Some("me@work.example (Max)"));
    assert_eq!(h.identity.display_label.as_deref(), Some("me@home.example (Pro)"));
    assert_eq!(w.usage.iter().map(|u| u.dedup_key.as_str()).collect::<Vec<_>>(), vec!["claude:msg_work"]);
    assert_eq!(h.usage.iter().map(|u| u.dedup_key.as_str()).collect::<Vec<_>>(), vec!["claude:msg_home"]);
    let keys: std::collections::HashSet<_> = batches.iter().flat_map(|b| &b.cursors).map(|c| &c.source_key).collect();
    assert_eq!(keys.len(), 2, "cursor keys never collide across folders");
}

#[tokio::test]
async fn folders_without_a_pro_max_or_ultra_login_are_never_read() {
    for settings in [
        Some(settings_json("uuid-team", "me@work.example", "org", "claude_team", SENTINEL)),
        Some(settings_json("uuid-ent", "me@corp.example", "org", "claude_enterprise", SENTINEL)),
        None, // API key or signed out: no oauthAccount
    ] {
        let home = Home::new();
        match &settings {
            Some(s) => std::fs::write(home.0.join(".claude.json"), s).unwrap(),
            None => std::fs::remove_file(home.0.join(".claude.json")).unwrap(),
        }
        let mut log = LogBuilder::new();
        log.assistant("s", "m", "claude-opus-4-5", T, tokens(1, 1, 0, 0));
        write_session(&home.default_dir(), "p", "s", &log);
        let c = collector(&home, vec![]);
        assert!(!c.plan_supported(&c.folders()[0]));
        assert!(matches!(c.detect(&Ctx::default()).await, Detection::NotInstalled), "nothing to read");
        assert!(collect_all(&c, &HashMap::new()).await.is_empty(), "no batch, no usage");
        assert!(c.triggers().iter().all(|t| !matches!(t, agentpulse_core::collector::Trigger::WatchPaths(p) if !p.is_empty())), "not watched");
    }
}

#[tokio::test]
async fn a_supported_folder_is_still_read_next_to_a_skipped_team_folder() {
    let home = Home::new();
    home.default_settings("uuid-team", "me@work.example", "claude_team");
    let personal = home.folder(".claude-personal");
    std::fs::write(personal.join(".claude.json"), settings_json("uuid-p", "me@home.example", "org-2", "claude_pro", SENTINEL)).unwrap();
    let mut log = LogBuilder::new();
    log.assistant("s", "m", "claude-opus-4-5", T, tokens(1, 1, 0, 0));
    write_session(&home.default_dir(), "p", "s", &log);
    let mut mine = LogBuilder::new();
    mine.assistant("h", "msg_home", "claude-opus-4-5", T, tokens(1, 1, 0, 0));
    write_session(&personal, "p", "h", &mine);
    let c = collector(&home, vec![personal]);
    let batches = collect_all(&c, &HashMap::new()).await;
    assert_eq!(batches.len(), 1, "only the Pro folder");
    assert_eq!(batches[0].identity.display_label.as_deref(), Some("me@home.example (Pro)"));
    assert_eq!(c.plan_label(&c.folders()[0]).as_deref(), Some("Team"));
}

#[tokio::test]
async fn incremental_reads_start_at_the_cursor_and_leave_partial_lines() {
    let home = Home::new();
    let mut log = LogBuilder::new();
    log.assistant("s", "m1", "claude-opus-4-5", T, tokens(1, 5, 0, 0));
    let path = write_session(&home.default_dir(), "p", "s", &log);
    let c = collector(&home, vec![]);
    let first = collect_all(&c, &HashMap::new()).await;
    let cursors = committed(&first);
    let first_seq = first[0].usage[0].source_seq;

    let again = collect_one(&c, &cursors).await;
    assert!(again.usage.is_empty() && again.cursors.is_empty(), "unchanged file is not read");

    // m1's next content block (fuller usage) + a new message + a partial line being written.
    log.assistant("s", "m1", "claude-opus-4-5", T, tokens(1, 50, 0, 0))
        .assistant("s", "m2", "claude-opus-4-5", T2, tokens(2, 2, 0, 0));
    let mut text = log.build();
    text.push_str("{\"type\":\"assistant\",\"message\":{\"id\":\"m3\"");
    std::fs::write(&path, &text).unwrap();
    let next = collect_one(&c, &cursors).await;
    let m1 = next.usage.iter().find(|u| u.dedup_key == "claude:m1").unwrap();
    assert_eq!(m1.tokens.output, Some(50));
    assert!(m1.source_seq > first_seq, "a later line revises the stored record");
    assert!(next.usage.iter().any(|u| u.dedup_key == "claude:m2"));
    assert!(!next.usage.iter().any(|u| u.dedup_key == "claude:m3"));
    assert_eq!(next.cursors[0].state.byte_offset as usize, log.build().len(), "partial line not consumed");
}

#[tokio::test]
async fn a_rewritten_file_is_read_again_from_the_start() {
    let home = Home::new();
    let mut log = LogBuilder::new();
    log.assistant("s", "m1", "claude-opus-4-5", T, tokens(1, 1, 0, 0))
        .assistant("s", "m2", "claude-opus-4-5", T2, tokens(1, 1, 0, 0));
    let path = write_session(&home.default_dir(), "p", "s", &log);
    let c = collector(&home, vec![]);
    let cursors = committed(&collect_all(&c, &HashMap::new()).await);
    let mut other = LogBuilder::new();
    other.assistant("s2", "x1", "claude-opus-4-5", T3, tokens(1, 1, 0, 0));
    std::fs::write(&path, other.build()).unwrap();
    let b = collect_one(&c, &cursors).await;
    assert_eq!(b.usage.iter().map(|u| u.dedup_key.as_str()).collect::<Vec<_>>(), vec!["claude:x1"]);
}

#[tokio::test]
async fn not_installed_without_any_logs_folder() {
    let home = Home::new();
    std::fs::remove_dir_all(home.default_dir()).unwrap();
    let c = collector(&home, vec![home.0.join(".claude-missing")]);
    assert!(matches!(c.detect(&Ctx::default()).await, Detection::NotInstalled));
}

#[tokio::test]
async fn folders_are_deduplicated_and_suggestions_offer_only_uncollected_claude_folders() {
    let home = Home::new();
    let personal = home.folder(".claude-personal");
    std::fs::write(personal.join(".claude.json"), settings_json("uuid-p", "me@home.example", "org", "claude_pro", SENTINEL)).unwrap();
    let team = home.folder(".claude-work");
    std::fs::write(team.join(".claude.json"), settings_json("uuid-t", "me@work.example", "org", "claude_team", SENTINEL)).unwrap();
    let _no_logs = std::fs::create_dir_all(home.0.join(".claude-empty"));
    let _unrelated = home.folder(".codex");
    let c = ClaudeCodeCollector::new(home.0.clone(), Some(home.default_dir()), vec![]);
    assert_eq!(c.folders().len(), 1, "CLAUDE_CONFIG_DIR pointing at the default isn't a second folder");
    assert_eq!(c.suggestions(), vec![personal.clone()]);
    let trailing = PathBuf::from(format!("{}/", personal.to_string_lossy()));
    c.set_added_folders(vec![trailing]);
    assert!(c.is_collected(&personal));
    assert!(c.suggestions().is_empty());
    assert_eq!(c.folders().len(), 2);
}

#[tokio::test]
async fn privacy_no_content_reaches_the_batch() {
    let home = Home::new();
    home.default_settings("uuid-SECRET-77", "me@work.example", "claude_pro");
    let mut log = LogBuilder::new();
    log.content_lines("s", SENTINEL, T).assistant("s", "msg_ok", "claude-opus-4-5", T2, tokens(1, 1, 0, 0));
    write_session(&home.default_dir(), &format!("C--projects-{SENTINEL}"), "s", &log);

    let b = collect_one(&collector(&home, vec![]), &HashMap::new()).await;
    assert!(b.usage.iter().any(|u| u.dedup_key == "claude:msg_ok"), "usage still collected");
    let dump = format!("{b:?}");
    assert!(!dump.contains(SENTINEL), "content leaked into the batch");
    assert!(!dump.contains("uuid-SECRET-77"), "account id leaked into the batch");
    assert!(!dump.contains("projects"), "a path leaked into the batch");
    assert!(!dump.contains(&home.0.to_string_lossy().to_string()), "home path leaked into the batch");
}

/// Collecting must never change anything in a Claude Code folder.
#[tokio::test]
async fn collecting_never_writes_to_the_config_folders() {
    let home = Home::new();
    home.default_settings("uuid-1", "me@work.example", "claude_pro");
    let personal = home.folder(".claude-personal");
    let mut log = LogBuilder::new();
    log.content_lines("s", SENTINEL, T).assistant("s", "m", "claude-opus-4-5", T2, tokens(1, 1, 0, 0));
    write_session(&home.default_dir(), "p", "s", &log);
    write_session(&personal, "p", "s2", &log);

    let before = snapshot(&home.0);
    let c = collector(&home, vec![personal]);
    let first = collect_all(&c, &HashMap::new()).await;
    let _ = c.detect(&Ctx::default()).await;
    let _ = c.suggestions();
    let _ = collect_all(&c, &committed(&first)).await;
    assert_eq!(snapshot(&home.0), before, "a file in the home or a config folder changed");
}

/// "Don't read on this machine": the default folder is dropped however it got in
/// (default, `CLAUDE_CONFIG_DIR`, added) and is never read, watched or suggested.
#[tokio::test]
async fn a_skipped_default_folder_is_not_listed_read_watched_or_suggested() {
    let home = Home::new();
    let personal = home.folder(".claude-personal");
    std::fs::write(personal.join(".claude.json"), settings_json("uuid-default", "me@default.example", "org-1", "claude_pro", SENTINEL)).unwrap();
    let mut log = LogBuilder::new();
    log.assistant("s", "m_default", "claude-opus-4-5", T, tokens(1, 1, 0, 0));
    write_session(&home.default_dir(), "p", "s", &log);
    let mut mine = LogBuilder::new();
    mine.assistant("h", "m_personal", "claude-opus-4-5", T, tokens(2, 2, 0, 0));
    write_session(&personal, "p", "h", &mine);

    // Also named by CLAUDE_CONFIG_DIR, to check it's dropped by path, not by origin.
    let c = ClaudeCodeCollector::new(home.0.clone(), Some(home.default_dir()), vec![personal.clone()]);
    assert_eq!(collect_all(&c, &HashMap::new()).await.len(), 2, "both read before");

    c.set_default_skipped(true);
    assert!(c.default_skipped());
    let paths: Vec<_> = c.folders().into_iter().map(|f| f.path).collect();
    assert_eq!(paths, vec![personal.clone()], "only the added folder is listed; added folders are kept");
    let batches = collect_all(&c, &HashMap::new()).await;
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].usage.iter().map(|u| u.dedup_key.as_str()).collect::<Vec<_>>(), vec!["claude:m_personal"]);
    let watched = c.triggers().into_iter().find_map(|t| match t {
        agentpulse_core::collector::Trigger::WatchPaths(p) => Some(p),
        _ => None,
    });
    assert!(!watched.unwrap().contains(&home.default_dir().join("projects")), "not watched");
    assert!(c.suggestions().is_empty(), "not offered as a suggestion either");

    c.set_default_skipped(false);
    assert_eq!(c.folders().len(), 2, "turned back on; the added folder is still there");
    assert_eq!(collect_all(&c, &HashMap::new()).await.len(), 2);
}

/// The session keys listed for deleting a folder's data are exactly the keys its
/// collected sessions are stored under, and listing them writes nothing.
#[tokio::test]
async fn session_keys_match_collected_sessions_and_listing_never_writes() {
    let home = Home::new();
    let mut a = LogBuilder::new();
    a.content_lines("s1", SENTINEL, T).assistant("s1", "m1", "claude-opus-4-5", T2, tokens(1, 1, 0, 0));
    write_session(&home.default_dir(), "p", "s1", &a);
    let mut b = LogBuilder::new();
    b.assistant("s2", "m2", "claude-opus-4-5", T3, tokens(1, 1, 0, 0));
    write_session(&home.default_dir(), "q", "s2", &b);

    let c = collector(&home, vec![]);
    let folder = c.folders().remove(0);
    let before = snapshot(&home.0);
    let mut keys = folder.session_keys().unwrap();
    assert_eq!(snapshot(&home.0), before, "listing session keys changed a file");
    let batch = collect_one(&c, &HashMap::new()).await;
    let mut collected: Vec<_> = batch.sessions.iter().map(|s| s.session_key.clone()).collect();
    keys.sort();
    collected.sort();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys, collected);
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

/// Manual check against the real Claude Code folders (read-only). Prints counts only.
/// Extra folders come from `AGENTPULSE_CLAUDE_DIRS` (separated by `;`).
/// Run with: cargo test -p agentpulse-collectors real_claude_folders -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn real_claude_folders_summary() {
    let home = home_dir();
    let added: Vec<PathBuf> = std::env::var("AGENTPULSE_CLAUDE_DIRS")
        .map(|v| v.split(';').filter(|s| !s.is_empty()).map(PathBuf::from).collect())
        .unwrap_or_default();
    let c = ClaudeCodeCollector::new(home, None, added);
    let started = std::time::Instant::now();
    let batches = collect_all(&c, &HashMap::new()).await;
    println!("folders: {}, elapsed: {:?}", batches.len(), started.elapsed());
    for (i, b) in batches.iter().enumerate() {
        let sum = |f: fn(&agentpulse_core::usage::TokenUsage) -> Option<u64>| {
            b.usage.iter().filter_map(|u| f(&u.tokens)).sum::<u64>()
        };
        println!(
            "folder {i}: scope {}, identity {:?}, files {}, sessions {}, usage records {}, contexts {}, version {:?}, valid {:?}",
            b.entitlement_scope,
            b.identity.kind,
            b.cursors.len(),
            b.sessions.len(),
            b.usage.len(),
            b.contexts.len(),
            b.detected_version,
            b.validate()
        );
        println!(
            "  tokens: input {} output {} cache_read {} cache_write {}",
            sum(|t| t.input),
            sum(|t| t.output),
            sum(|t| t.cache_read),
            sum(|t| t.cache_write)
        );
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(".").to_path_buf())
}

/// Read-only: which real folders are supported. Prints plan names only.
#[tokio::test]
#[ignore]
async fn real_claude_folder_plans() {
    let home = home_dir();
    let c = ClaudeCodeCollector::new(home.clone(), None, vec![home.join(".claude-personal")]);
    for f in c.folders() {
        println!("{:?}: has_logs={} supported={} plan={:?}", f.origin, f.has_logs(), c.plan_supported(&f), c.plan_label(&f));
    }
}
