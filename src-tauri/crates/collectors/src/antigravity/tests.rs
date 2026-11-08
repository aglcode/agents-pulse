#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use agentpulse_core::batch::{Availability, CursorState, NormalizedBatch, UsageEvent};
use agentpulse_core::collector::{Collector, Ctx, Detection, Trigger};
use agentpulse_core::hash::stable_hash;
use agentpulse_core::identity::IdentityKind;
use agentpulse_core::metric::{SourceKind, UnavailableReason};
use tokio_util::sync::CancellationToken;

use super::testkit::{AUX_MODEL, CLAUDE_OPUS, Conversation, GEMINI_PRO, Tokens, forbidden_files, write_conversation};
use super::{AntigravityCollector, SCOPE};

const SENTINEL: &str = "SENTINEL-4ntigrav-PRIVATE-77";
const T0: i64 = 1_762_000_000; // seconds
const IDE: &str = "antigravity-ide";
const APP: &str = "antigravity";

/// A fake `~/.gemini`.
struct Gemini(PathBuf);

impl Gemini {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "agentpulse-antigravity-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn folder(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Gemini {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tok(input: u64, cache_read: u64, output: u64, thinking: u64) -> Tokens {
    Tokens { input, output, cache_read, cache_write: 0, thinking }
}

async fn collect_all(c: &AntigravityCollector, cursors: &HashMap<String, CursorState>) -> Vec<NormalizedBatch> {
    let ctx = Ctx { home_dir: None, cursors: cursors.clone(), installs: vec![] };
    c.collect(&ctx, &CancellationToken::new()).await.unwrap()
}

async fn collect(c: &AntigravityCollector, cursors: &HashMap<String, CursorState>) -> NormalizedBatch {
    let mut v = collect_all(c, cursors).await;
    assert_eq!(v.len(), 1);
    v.remove(0)
}

fn committed(b: &NormalizedBatch) -> HashMap<String, CursorState> {
    b.cursors.iter().map(|c| (c.source_key.clone(), c.state.clone())).collect()
}

fn by_tokens(b: &NormalizedBatch, input: u64) -> &UsageEvent {
    b.usage.iter().find(|u| u.tokens.input == Some(input)).expect("usage record")
}

#[tokio::test]
async fn calls_are_counted_once_from_steps_with_names_from_generation_records() {
    let g = Gemini::new();
    let mut conv = Conversation::new(SENTINEL);
    conv.plain_step(T0)
        .call("r1", GEMINI_PRO, Some("gemini-pro-default"), T0 + 1, tok(100, 900, 50, 20))
        .plain_step(T0 + 2)
        .call("r2", CLAUDE_OPUS, Some("claude-opus-4-6-thinking"), T0 + 3, Tokens { cache_write: 30, ..tok(200, 0, 7, 0) });
    write_conversation(&g.folder(IDE), "conv-1", &conv);

    let b = collect(&AntigravityCollector::new(g.0.clone()), &HashMap::new()).await;
    assert_eq!(b.usage.len(), 2, "generation records repeat the steps' usage and aren't counted");
    let r1 = by_tokens(&b, 100);
    assert_eq!(r1.tokens.cache_read, Some(900), "input is fresh only; cache reads are separate");
    assert_eq!(r1.tokens.output, Some(50), "output already includes thinking");
    assert_eq!(r1.tokens.reasoning, Some(20));
    assert_eq!(r1.tokens.cache_write, None);
    assert_eq!(r1.model.as_deref(), Some("gemini-pro-default"), "the API model id, not the display name");
    assert_eq!(r1.occurred_at, (T0 + 1) * 1000 + 500);
    assert_eq!(r1.source, SourceKind::Local);
    let r2 = by_tokens(&b, 200);
    assert_eq!(r2.tokens.cache_write, Some(30));
    assert_eq!(r2.tokens.reasoning, None, "not reported");
    assert_eq!(r2.model.as_deref(), Some("claude-opus-4-6-thinking"));
    assert!(b.usage.iter().all(|u| u.dedup_key.starts_with("antigravity:") && !u.dedup_key.contains("r1")));

    let session = stable_hash(&["antigravity-session", &b.install_key, "conv-1"]);
    assert!(b.usage.iter().all(|u| u.session_key.as_deref() == Some(session.as_str())));
    assert_eq!(b.sessions.len(), 1);
    assert_eq!(b.sessions[0].started_at, T0 * 1000 + 500, "first step, with or without usage");
    assert_eq!(b.sessions[0].last_activity_at, (T0 + 3) * 1000 + 500);
    assert_eq!(b.sessions[0].model.as_deref(), Some("claude-opus-4-6-thinking"));
    assert_eq!(b.contexts.len(), 1);
    assert_eq!(b.contexts[0].used_tokens, 230, "latest call's prompt: fresh + cache read + cache write");
    assert_eq!(b.contexts[0].total_tokens, None, "never guessed");
    assert_eq!(b.contexts[0].total_source, SourceKind::Unavailable);
    assert_eq!(b.entitlement_scope, SCOPE);
    assert_eq!(b.identity.kind, IdentityKind::None, "install-scoped");
    assert!(b.validate().is_ok());
}

#[tokio::test]
async fn auxiliary_calls_count_without_a_name_and_dont_drive_context() {
    let g = Gemini::new();
    let mut conv = Conversation::new(SENTINEL);
    conv.call("r1", GEMINI_PRO, Some("gemini-pro-default"), T0, tok(1000, 0, 5, 0)).aux_call("aux", T0 + 9, tok(77, 0, 3, 0));
    write_conversation(&g.folder(IDE), "c", &conv);

    let b = collect(&AntigravityCollector::new(g.0.clone()), &HashMap::new()).await;
    assert_eq!(b.usage.len(), 2, "an auxiliary call is real usage");
    let aux = by_tokens(&b, 77);
    assert_eq!(aux.model, None, "model {AUX_MODEL} has no generation record, so no name");
    assert_eq!(b.contexts[0].used_tokens, 1000, "the latest call of the conversation itself");
    assert_eq!(b.sessions[0].last_activity_at, (T0 + 9) * 1000 + 500);
}

#[tokio::test]
async fn a_model_without_a_name_in_its_own_conversation_stays_unnamed() {
    let g = Gemini::new();
    let mut named = Conversation::new(SENTINEL);
    named.call("a", GEMINI_PRO, Some("gemini-pro-default"), T0, tok(1, 0, 1, 0));
    write_conversation(&g.folder(IDE), "named", &named);
    let mut unnamed = Conversation::new(SENTINEL);
    unnamed.call("b", GEMINI_PRO, None, T0, tok(2, 0, 1, 0));
    write_conversation(&g.folder(IDE), "unnamed", &unnamed);

    let b = collect(&AntigravityCollector::new(g.0.clone()), &HashMap::new()).await;
    assert_eq!(by_tokens(&b, 1).model.as_deref(), Some("gemini-pro-default"));
    assert_eq!(by_tokens(&b, 2).model, None, "names are per conversation, so a later run can't rename it");
}

#[tokio::test]
async fn the_api_model_id_wins_over_a_display_name_in_the_same_conversation() {
    let g = Gemini::new();
    let mut both = Conversation::new(SENTINEL);
    both.call("a", CLAUDE_OPUS, None, T0, tok(1, 0, 1, 0))
        .display_only_record(CLAUDE_OPUS, "Claude Opus 4.6 (Thinking)")
        .display_only_record(CLAUDE_OPUS, "Claude Opus 4.6 (Thinking)")
        .call("b", CLAUDE_OPUS, Some("claude-opus-4-6-thinking"), T0 + 1, tok(2, 0, 1, 0));
    write_conversation(&g.folder(IDE), "both", &both);
    let mut display_only = Conversation::new(SENTINEL);
    display_only.call("c", CLAUDE_OPUS, None, T0, tok(3, 0, 1, 0)).display_only_record(CLAUDE_OPUS, "Claude Opus 4.6 (Thinking)");
    write_conversation(&g.folder(IDE), "display", &display_only);

    let b = collect(&AntigravityCollector::new(g.0.clone()), &HashMap::new()).await;
    assert_eq!(by_tokens(&b, 1).model.as_deref(), Some("claude-opus-4-6-thinking"), "even when display names are more common");
    assert_eq!(by_tokens(&b, 2).model.as_deref(), Some("claude-opus-4-6-thinking"));
    assert_eq!(by_tokens(&b, 3).model.as_deref(), Some("Claude Opus 4.6 (Thinking)"), "the only name this conversation gives");
}

#[tokio::test]
async fn changes_still_in_the_write_ahead_log_are_read() {
    let g = Gemini::new();
    let mut conv = Conversation::new(SENTINEL);
    for i in 0..30u64 {
        conv.call(&format!("r{i}"), GEMINI_PRO, Some("gemini-pro-default"), T0 + i as i64, tok(i + 1, 0, 1, 0));
    }
    let path = g.folder(IDE).join("conversations").join("live.db");
    conv.write_with_pending_wal(&path, 20);
    assert!(super::files::wal_path(&path).metadata().unwrap().len() > 0, "test setup: a pending log");

    let b = collect(&AntigravityCollector::new(g.0.clone()), &HashMap::new()).await;
    assert_eq!(b.usage.len(), 30, "10 checkpointed calls + 20 only in the log");
    assert_eq!(b.contexts[0].used_tokens, 30);
}

#[tokio::test]
async fn unchanged_databases_are_skipped_and_changed_ones_never_double_count() {
    let g = Gemini::new();
    let mut conv = Conversation::new(SENTINEL);
    conv.call("r1", GEMINI_PRO, Some("gemini-pro-default"), T0, tok(10, 0, 1, 0));
    let path = write_conversation(&g.folder(IDE), "c", &conv);
    let c = AntigravityCollector::new(g.0.clone());
    let first = collect(&c, &HashMap::new()).await;
    let cursors = committed(&first);

    let again = collect(&c, &cursors).await;
    assert!(again.usage.is_empty() && again.cursors.is_empty(), "unchanged database is not read");

    conv.call("r2", GEMINI_PRO, Some("gemini-pro-default"), T0 + 5, tok(20, 0, 1, 0));
    std::thread::sleep(std::time::Duration::from_millis(20));
    conv.write(&path);
    let next = collect(&c, &cursors).await;
    assert_eq!(next.usage.len(), 2, "the whole conversation is re-read");
    assert_eq!(by_tokens(&next, 10).dedup_key, first.usage[0].dedup_key, "same call, same key: the store keeps one");
    assert_eq!(by_tokens(&next, 10).source_seq, first.usage[0].source_seq, "unchanged record isn't a revision");
}

#[tokio::test]
async fn each_data_folder_is_its_own_install_and_the_backup_is_ignored() {
    let g = Gemini::new();
    let mut a = Conversation::new(SENTINEL);
    a.call("same-id", GEMINI_PRO, Some("gemini-pro-default"), T0, tok(1, 0, 1, 0));
    write_conversation(&g.folder(IDE), "x", &a);
    write_conversation(&g.folder(APP), "x", &a);
    forbidden_files(&g.0, IDE, SENTINEL);

    let c = AntigravityCollector::new(g.0.clone());
    let batches = collect_all(&c, &HashMap::new()).await;
    assert_eq!(batches.len(), 2, "IDE and app; never the backup folder");
    assert_ne!(batches[0].install_key, batches[1].install_key);
    assert_ne!(batches[0].usage[0].dedup_key, batches[1].usage[0].dedup_key, "installs never share records");
    assert!(batches.iter().all(|b| b.usage.len() == 1));
    let Detection::Installed(installs) = c.detect(&Ctx::default()).await else { panic!("installed") };
    assert_eq!(installs.len(), 2);
    let watched: Vec<_> = c.triggers().into_iter().filter_map(|t| if let Trigger::WatchPaths(p) = t { Some(p) } else { None }).collect();
    assert_eq!(watched[0].len(), 2);
    assert!(watched[0].iter().all(|p| p.ends_with("conversations")));
}

#[tokio::test]
async fn unreadable_and_unknown_records_are_skipped_without_hiding_the_rest() {
    let g = Gemini::new();
    let mut conv = Conversation::new(SENTINEL);
    conv.call("ok", GEMINI_PRO, Some("gemini-pro-default"), T0, tok(5, 0, 1, 0))
        .garbage_step()
        .future_format_call("future", T0 + 1, tok(6, 0, 1, 0));
    write_conversation(&g.folder(IDE), "mixed", &conv);
    let conversations = g.folder(IDE).join("conversations");
    std::fs::write(conversations.join("corrupt.db"), b"not a database at all, just bytes").unwrap();

    let b = collect(&AntigravityCollector::new(g.0.clone()), &HashMap::new()).await;
    assert_eq!(b.usage.len(), 1, "only the known, valid record");
    assert_eq!(by_tokens(&b, 5).model.as_deref(), Some("gemini-pro-default"));
    let tokens = b.availability.iter().find(|a| a.metric == "tokens").unwrap();
    assert!(matches!(tokens.availability, Availability::Available { .. }));
}

#[tokio::test]
async fn an_unknown_database_layout_is_reported_as_unsupported() {
    let g = Gemini::new();
    let path = g.folder(IDE).join("conversations").join("new.db");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE turns(id INTEGER PRIMARY KEY, usage BLOB);").unwrap();
    drop(c);

    let b = collect(&AntigravityCollector::new(g.0.clone()), &HashMap::new()).await;
    assert!(b.usage.is_empty());
    let tokens = b.availability.iter().find(|a| a.metric == "tokens").unwrap();
    assert!(matches!(&tokens.availability, Availability::Unavailable(UnavailableReason::UnsupportedVersion { .. })));
    assert!(b.cursors.is_empty(), "retried on later runs");
}

#[tokio::test]
async fn quota_is_unavailable() {
    let g = Gemini::new();
    write_conversation(&g.folder(IDE), "c", &Conversation::new(SENTINEL));
    let b = collect(&AntigravityCollector::new(g.0.clone()), &HashMap::new()).await;
    assert!(b.quotas.is_empty());
    let quota = b.availability.iter().find(|a| a.metric == "quota").unwrap();
    assert_eq!(quota.availability, Availability::Unavailable(UnavailableReason::NotExposedByProvider));
    assert!(b.sessions.is_empty(), "an empty conversation has no activity");
}

#[tokio::test]
async fn not_installed_without_a_conversations_folder() {
    let g = Gemini::new();
    std::fs::create_dir_all(g.folder(IDE)).unwrap();
    std::fs::create_dir_all(g.folder("antigravity-backup").join("conversations")).unwrap();
    let c = AntigravityCollector::new(g.0.clone());
    assert!(matches!(c.detect(&Ctx::default()).await, Detection::NotInstalled));
    assert!(collect_all(&c, &HashMap::new()).await.is_empty());
    assert!(c.triggers().iter().all(|t| !matches!(t, Trigger::WatchPaths(p) if !p.is_empty())));
}

#[tokio::test]
async fn privacy_no_content_ids_paths_or_secrets_reach_the_batch() {
    let g = Gemini::new();
    forbidden_files(&g.0, IDE, SENTINEL);
    let mut conv = Conversation::new(SENTINEL);
    conv.plain_step(T0)
        .call(&format!("resp-{SENTINEL}"), GEMINI_PRO, Some("gemini-pro-default"), T0 + 1, tok(10, 0, 1, 0))
        .aux_call(&format!("aux-{SENTINEL}"), T0 + 2, tok(3, 0, 1, 0));
    // The conversation id is the file name.
    write_conversation(&g.folder(IDE), &format!("conv-{SENTINEL}"), &conv);

    let b = collect(&AntigravityCollector::new(g.0.clone()), &HashMap::new()).await;
    assert_eq!(b.usage.len(), 2, "usage still collected; the backup database is not");
    let dump = format!("{b:?}");
    assert!(!dump.contains(SENTINEL), "content, an id, a path or a secret leaked into the batch");
    assert!(!dump.contains("conversations"), "a path leaked into the batch");
    assert!(!dump.contains(&g.0.to_string_lossy().to_string()), "a path leaked into the batch");
}

/// Collecting must never create, change or delete anything in Antigravity's folders:
/// no `-shm`, no journal, no copy, even while a write-ahead log is pending.
#[tokio::test]
async fn collecting_never_writes_to_antigravity_folders() {
    let g = Gemini::new();
    forbidden_files(&g.0, IDE, SENTINEL);
    let mut conv = Conversation::new(SENTINEL);
    for i in 0..5 {
        conv.call(&format!("r{i}"), GEMINI_PRO, Some("gemini-pro-default"), T0 + i, tok(1, 0, 1, 0));
    }
    conv.write_with_pending_wal(&g.folder(IDE).join("conversations").join("live.db"), 3);
    write_conversation(&g.folder(APP), "plain", &conv);
    std::fs::write(g.folder(APP).join("conversations").join("corrupt.db"), b"junk").unwrap();

    let before = snapshot(&g.0);
    let c = AntigravityCollector::new(g.0.clone());
    let first = collect_all(&c, &HashMap::new()).await;
    let _ = c.detect(&Ctx::default()).await;
    let _ = c.triggers();
    let cursors: HashMap<_, _> = first.iter().flat_map(committed).collect();
    let _ = collect_all(&c, &cursors).await;
    assert_eq!(snapshot(&g.0), before, "a file in Antigravity's folders was created, changed or deleted");
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
                let content = std::fs::read(&p).unwrap();
                let hash = stable_hash(&[&String::from_utf8_lossy(&content)]);
                out.insert(p.to_string_lossy().to_string(), (m.len(), m.modified().unwrap(), hash));
            }
        }
    }
    out
}

/// Manual check against the real `~/.gemini` (read-only). Prints counts, models and
/// token sums only. Run with:
/// cargo test -p agentpulse-collectors real_antigravity -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn real_antigravity_summary() {
    let user_home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap();
    let c = AntigravityCollector::new(AntigravityCollector::default_dir(&user_home));
    let started = std::time::Instant::now();
    let batches = collect_all(&c, &HashMap::new()).await;
    println!("elapsed {:?}; installs {}", started.elapsed(), batches.len());
    for b in &batches {
        let sum = |f: fn(&agentpulse_core::usage::TokenUsage) -> Option<u64>| b.usage.iter().filter_map(|u| f(&u.tokens)).sum::<u64>();
        let mut models: BTreeMap<String, usize> = BTreeMap::new();
        for u in &b.usage {
            *models.entry(u.model.clone().unwrap_or_else(|| "<unnamed>".into())).or_default() += 1;
        }
        println!(
            "install {}: files {}, sessions {}, usage records {}, contexts {}, valid {:?}",
            &b.install_key[..8],
            b.cursors.len(),
            b.sessions.len(),
            b.usage.len(),
            b.contexts.len(),
            b.validate()
        );
        println!(
            "  tokens: input {} output {} cache_read {} cache_write {} reasoning {}",
            sum(|t| t.input),
            sum(|t| t.output),
            sum(|t| t.cache_read),
            sum(|t| t.cache_write),
            sum(|t| t.reasoning)
        );
        println!("  records per model: {models:?}");
        println!("  availability: {:?}", b.availability.iter().map(|a| (&a.metric, &a.availability)).collect::<Vec<_>>());
    }
}
