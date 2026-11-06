//! M4b tests: settings edits, the tap, and quota collection. Everything runs in
//! temporary folders; no test touches a real Claude Code folder.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use agentpulse_core::batch::Availability;
use agentpulse_core::collector::{Collector, Ctx};
use agentpulse_core::hash::stable_hash;
use agentpulse_core::metric::{SourceKind, Stability, UnavailableReason};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::ClaudeCodeCollector;
use super::settings_edit::{self, EditError, TapState};
use super::tap::{self, Snapshot};
use super::testkit::{LogBuilder, Tokens, write_session};

const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CMD: &str = "~/AppData/Local/AgentPulse/agents-pulse.exe --statusline-tap 0123456789abcdef";
const SENTINEL: &str = "SENTINEL-tap-PRIVATE-5";

/// The user's real `.claude-personal/settings.json` text (as pasted on 2026-11-06), used as a fixture.
const USER_SETTINGS: &str = "{\n  \"model\": \"opus\",\n  \"modelSettings\": {\n    \"claude-opus-5-5\": {\n      \"effortLevel\": \"high\"\n    }\n  },\n  \"theme\": \"dark\",\n  \"enabledPlugins\": {\n    \"rust-analyzer-lsp@claude-plugins-official\": true\n  }\n}\n";

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!("agentpulse-tap-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn settings(&self) -> PathBuf {
        self.0.join("settings.json")
    }
    fn write(&self, text: &str) {
        std::fs::write(self.settings(), text).unwrap();
    }
    fn read(&self) -> String {
        std::fs::read_to_string(self.settings()).unwrap()
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn enable(dir: &Path) {
    settings_edit::apply(&settings_edit::plan_enable(dir, CMD).unwrap()).unwrap();
}

#[test]
fn enable_adds_only_the_block_and_disable_restores_the_file_byte_for_byte() {
    let t = Tmp::new();
    t.write(USER_SETTINGS);
    assert_eq!(settings_edit::tap_state(&t.0), TapState::Off);

    let plan = settings_edit::plan_enable(&t.0, CMD).unwrap();
    assert_eq!(plan.before.as_deref(), Some(USER_SETTINGS));
    assert_eq!(std::fs::read_to_string(t.settings()).unwrap(), USER_SETTINGS, "preview writes nothing");
    settings_edit::apply(&plan).unwrap();
    let on = t.read();
    let expected_tail = format!(
        "  }},\n  \"statusLine\": {{\n    \"type\": \"command\",\n    \"command\": \"{CMD}\"\n  }}\n}}\n"
    );
    assert!(on.ends_with(&expected_tail), "block appended in Claude Code's own format:\n{on}");
    assert!(on.starts_with(&USER_SETTINGS[..USER_SETTINGS.len() - 4]), "everything before it unchanged");
    assert_eq!(settings_edit::tap_state(&t.0), TapState::On);
    assert!(!t.0.join("settings.json.agentpulse-tmp").exists(), "no temp file left behind");

    settings_edit::apply(&settings_edit::plan_disable(&t.0).unwrap()).unwrap();
    assert_eq!(t.read(), USER_SETTINGS, "disable restores the original exactly");
    assert_eq!(settings_edit::tap_state(&t.0), TapState::Off);
}

#[test]
fn crlf_files_round_trip() {
    let t = Tmp::new();
    let crlf = USER_SETTINGS.replace('\n', "\r\n");
    t.write(&crlf);
    enable(&t.0);
    assert!(!t.read().replace("\r\n", "").contains('\n'), "inserted lines use CRLF too");
    settings_edit::apply(&settings_edit::plan_disable(&t.0).unwrap()).unwrap();
    assert_eq!(t.read(), crlf);
}

/// After `/model`, Claude Code re-saves the file with `JSON.stringify(settings, null, 2)`,
/// which keeps key order: changed values in place, new keys at the end. The tap must
/// still be on, and disabling must remove only its block.
#[test]
fn survives_claude_code_resaving_after_a_model_change() {
    let t = Tmp::new();
    t.write(USER_SETTINGS);
    enable(&t.0);
    let on = t.read();
    let resave = |s: &str| {
        s.replace("\"model\": \"opus\"", "\"model\": \"sonnet\"")
            .replace("\"effortLevel\": \"high\"", "\"effortLevel\": \"medium\"")
    };
    // New key appended after statusLine, in insertion order.
    let resaved = resave(&on).replacen("\n  }\n}\n", "\n  },\n  \"permissions\": {\n    \"allow\": []\n  }\n}\n", 1);
    assert!(resaved.ends_with("\"permissions\": {\n    \"allow\": []\n  }\n}\n"));
    t.write(&resaved);
    assert_eq!(settings_edit::tap_state(&t.0), TapState::On, "still on after the re-save");

    settings_edit::apply(&settings_edit::plan_disable(&t.0).unwrap()).unwrap();
    let expected = resave(USER_SETTINGS)
        .replacen("\n  }\n}\n", "\n  },\n  \"permissions\": {\n    \"allow\": []\n  }\n}\n", 1);
    assert_eq!(t.read(), expected, "only the block removed; the model change and new key are kept");
}

/// A tool that re-saves with sorted keys moves the block mid-file and reorders it.
#[test]
fn disable_also_handles_a_sorted_key_resave() {
    let t = Tmp::new();
    t.write(USER_SETTINGS);
    enable(&t.0);
    let mut v: Value = serde_json::from_str(&t.read()).unwrap();
    v["model"] = json!("sonnet");
    v["modelSettings"]["claude-opus-5-5"]["effortLevel"] = json!("medium");
    // A key added later. serde_json sorts keys, so this re-save also moves statusLine
    // into the middle of the file: a harder case than Claude Code's insertion order.
    v.as_object_mut().unwrap().insert("permissions".into(), json!({ "allow": [] }));
    let resaved = js_stringify(&v);
    t.write(&resaved);
    assert_eq!(settings_edit::tap_state(&t.0), TapState::On, "still on after the re-save");

    settings_edit::apply(&settings_edit::plan_disable(&t.0).unwrap()).unwrap();
    let mut expected = v.clone();
    expected.as_object_mut().unwrap().remove("statusLine");
    assert_eq!(t.read(), js_stringify(&expected), "only the block removed; the model change is kept");
}

#[test]
fn never_replaces_the_users_own_status_line_and_never_enables_twice() {
    let t = Tmp::new();
    t.write("{\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"my-script.sh\"\n  }\n}\n");
    assert_eq!(settings_edit::tap_state(&t.0), TapState::OtherStatusLine);
    assert_eq!(settings_edit::plan_enable(&t.0, CMD), Err(EditError::OtherStatusLine));
    assert_eq!(settings_edit::plan_disable(&t.0), Err(EditError::OtherStatusLine));

    let t2 = Tmp::new();
    t2.write(USER_SETTINGS);
    enable(&t2.0);
    assert_eq!(settings_edit::plan_enable(&t2.0, CMD), Err(EditError::AlreadyOn));
}

#[test]
fn applying_refuses_if_the_file_changed_after_the_preview() {
    let t = Tmp::new();
    t.write(USER_SETTINGS);
    let plan = settings_edit::plan_enable(&t.0, CMD).unwrap();
    let changed = USER_SETTINGS.replace("\"opus\"", "\"sonnet\"");
    t.write(&changed); // e.g. a /model change saved in between
    assert_eq!(settings_edit::apply(&plan), Err(EditError::Changed));
    assert_eq!(t.read(), changed, "the user's change is untouched");
}

#[test]
fn missing_empty_invalid_and_reformatted_files() {
    let t = Tmp::new();
    enable(&t.0); // no settings.json yet
    assert_eq!(settings_edit::tap_state(&t.0), TapState::On);
    settings_edit::apply(&settings_edit::plan_disable(&t.0).unwrap()).unwrap();
    assert_eq!(t.read(), "{}\n");

    let empty = Tmp::new();
    empty.write("{}\n");
    enable(&empty.0);
    settings_edit::apply(&settings_edit::plan_disable(&empty.0).unwrap()).unwrap();
    assert_eq!(empty.read(), "{}\n");

    let bad = Tmp::new();
    bad.write("{ not json");
    assert_eq!(settings_edit::tap_state(&bad.0), TapState::Unreadable);
    assert_eq!(settings_edit::plan_enable(&bad.0, CMD), Err(EditError::InvalidJson));

    let squashed = Tmp::new();
    squashed.write(&format!("{{\"theme\":\"dark\",\"statusLine\":{{\"type\":\"command\",\"command\":\"{CMD}\"}}}}"));
    assert_eq!(settings_edit::tap_state(&squashed.0), TapState::On);
    assert_eq!(settings_edit::plan_disable(&squashed.0), Err(EditError::Reformatted), "never guesses");
}

/// The documented status line input, with every non-numeric field seeded with a sentinel.
fn documented_input(session: &str) -> String {
    json!({
        "hook_event_name": "Status",
        "session_id": session,
        "transcript_path": format!("C:/Users/x/.claude/projects/{SENTINEL}/s.jsonl"),
        "cwd": format!("C:/work/{SENTINEL}"),
        "model": { "id": "claude-opus-5-5", "display_name": "Opus" },
        "workspace": { "current_dir": format!("C:/work/{SENTINEL}"), "project_dir": SENTINEL },
        "version": "2.1.318",
        "output_style": { "name": "default" },
        "cost": { "total_cost_usd": 0.01234, "total_duration_ms": 45000, "total_lines_added": 156 },
        "context_window": {
            "total_input_tokens": 15500, "total_output_tokens": 4521, "context_window_size": 200000,
            "used_percentage": 8, "remaining_percentage": 92,
            "current_usage": { "input_tokens": 8500, "output_tokens": 1200,
                               "cache_creation_input_tokens": 5000, "cache_read_input_tokens": 2000 }
        },
        "rate_limits": {
            "five_hour": { "used_percentage": 23.5, "resets_at": 1738425600 },
            "seven_day": { "used_percentage": 41.2, "resets_at": 1738857600 }
        }
    })
    .to_string()
}

#[test]
fn tap_keeps_numbers_and_a_hashed_session_only() {
    let s = tap::capture(KEY, documented_input("sess-1").as_bytes(), 1_000).unwrap();
    assert_eq!(s.five_hour.unwrap().used_pct, 23.5);
    assert_eq!(s.five_hour.unwrap().resets_at, Some(1_738_425_600_000), "seconds → ms");
    assert_eq!(s.seven_day.unwrap().used_pct, 41.2);
    assert_eq!(s.context.unwrap().used_tokens, 15_500);
    assert_eq!(s.context.unwrap().window_tokens, 200_000);
    assert_eq!(s.session_key.as_deref(), Some(stable_hash(&["claude-session", KEY, "sess-1"]).as_str()));
    assert_eq!(tap::status_text(&s), "5h 24% · 7d 41% · ctx 8%");

    let dir = Tmp::new();
    let mut out = Vec::new();
    assert_eq!(tap::run(KEY, &dir.0, documented_input("sess-1").as_bytes(), &mut out), 0);
    assert_eq!(String::from_utf8(out).unwrap(), "5h 24% · 7d 41% · ctx 8%");
    let stored = std::fs::read_to_string(tap::snapshot_path(&dir.0, KEY)).unwrap();
    assert!(!stored.contains(SENTINEL), "a path or name leaked into the snapshot");
    assert!(!stored.contains("sess-1"), "session id stored in clear");
    assert!(!stored.contains("cost") && !stored.contains("0.01234"), "only quota and context are kept");
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1, "one file, overwritten each time");
}

#[test]
fn tap_fails_silently_on_bad_input_or_key() {
    let dir = Tmp::new();
    let mut out = Vec::new();
    assert_eq!(tap::run(KEY, &dir.0, &b"not json"[..], &mut out), 0);
    assert_eq!(tap::run("../../evil", &dir.0, documented_input("s").as_bytes(), &mut out), 0);
    assert!(out.is_empty(), "prints nothing");
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0, "writes nothing");

    // Before the first reply, or on a plan without limits: no rate_limits at all.
    let s = tap::capture(KEY, &br#"{"session_id":"s","context_window":{"total_input_tokens":0,"context_window_size":200000}}"#[..], 1).unwrap();
    assert!(s.five_hour.is_none() && s.seven_day.is_none());
    assert_eq!(tap::status_text(&s), "ctx 0%");
}

#[test]
fn tap_command_uses_forward_slashes_and_home_tilde() {
    let home = Path::new("C:/Users/Angelo Almonte");
    let exe = Path::new("C:/Users/Angelo Almonte/AppData/Local/AgentPulse/agents-pulse.exe");
    assert_eq!(
        tap::command_for(exe, home, KEY).unwrap(),
        format!("~/AppData/Local/AgentPulse/agents-pulse.exe --statusline-tap {KEY}")
    );
    assert!(tap::command_for(Path::new("C:/Program Files/AgentPulse/a.exe"), home, KEY).is_none(), "a space can't be run safely");
    assert!(tap::command_for(exe, home, "not-a-key").is_none());
    assert_eq!(tap::tap_key(&["app".into(), "--statusline-tap".into(), KEY.into()]), Some(KEY));
    assert_eq!(tap::tap_key(&["app".into()]), None);
}

// ---- collector integration ----

/// A fake user home with a default `.claude` folder and a tap folder.
fn home() -> Tmp {
    let t = Tmp::new();
    std::fs::create_dir_all(t.0.join(".claude/projects")).unwrap();
    std::fs::create_dir_all(t.0.join("tap")).unwrap();
    // Signed in to a plan Claude Code reports quota for.
    std::fs::write(t.0.join(".claude.json"), super::testkit::settings_json("uuid-pro", "me@home.example", "org", "claude_pro", SENTINEL)).unwrap();
    t
}

async fn collect(c: &ClaudeCodeCollector) -> agentpulse_core::batch::NormalizedBatch {
    let ctx = Ctx { home_dir: None, cursors: HashMap::new(), installs: vec![] };
    c.collect(&ctx, &CancellationToken::new()).await.unwrap().remove(0)
}

fn quota_availability(b: &agentpulse_core::batch::NormalizedBatch) -> Availability {
    b.availability.iter().find(|a| a.metric == "quota").unwrap().availability.clone()
}

#[tokio::test]
async fn with_the_tap_on_quota_and_exact_context_are_collected() {
    let home = home();
    let dir = home.0.join(".claude");
    let mut log = LogBuilder::new();
    log.assistant("sess-1", "m1", "claude-opus-5-5", "2026-11-04T01:00:00.000Z",
        Tokens { input: 500, output: 10, cache_read: 15_000, cache_write: 0 });
    write_session(&dir, "p", "sess-1", &log);
    let c = ClaudeCodeCollector::new(home.0.clone(), None, vec![]).with_tap_dir(home.0.join("tap"));
    let key = c.folders()[0].key();

    // Off: the tap's absence is explained, never shown as 0.
    let b = collect(&c).await;
    assert_eq!(quota_availability(&b), Availability::Unavailable(UnavailableReason::RequiresToolSettingsChange));

    // On, but Claude Code hasn't run the tap yet.
    enable(&dir);
    let b = collect(&c).await;
    assert_eq!(quota_availability(&b), Availability::Unavailable(UnavailableReason::NotYetCollected));

    // Claude Code runs the tap.
    tap::run(&key, &home.0.join("tap"), documented_input("sess-1").as_bytes(), std::io::sink());
    let b = collect(&c).await;
    assert_eq!(quota_availability(&b), Availability::Available { source: SourceKind::Local, stability: Stability::Documented });
    let kinds: Vec<_> = b.quotas.iter().map(|q| (q.window_kind.as_str(), q.used_pct)).collect();
    assert_eq!(kinds, vec![("5h", 23.5), ("7d", 41.2)]);
    assert!(b.quotas.iter().all(|q| q.stability == Stability::Documented));
    let session = stable_hash(&["claude-session", &key, "sess-1"]);
    let ctx: Vec<_> = b.contexts.iter().filter(|x| x.session_key == session).collect();
    assert_eq!(ctx.len(), 1, "the tap's exact context replaces the log estimate");
    assert_eq!(ctx[0].total_tokens, Some(200_000));
    assert_eq!(ctx[0].total_source, SourceKind::Local, "real window size, not Estimated");
    assert!(b.validate().is_ok());

    // Same snapshot again: nothing re-emitted, still available.
    let again = collect(&c).await;
    assert!(again.quotas.is_empty());
    assert!(matches!(quota_availability(&again), Availability::Available { .. }));

    // Turned off again: old snapshot ignored.
    settings_edit::apply(&settings_edit::plan_disable(&dir).unwrap()).unwrap();
    let off = collect(&c).await;
    assert!(off.quotas.is_empty());
    assert_eq!(quota_availability(&off), Availability::Unavailable(UnavailableReason::RequiresToolSettingsChange));
}

#[test]
fn snapshots_from_another_version_are_ignored() {
    let dir = Tmp::new();
    let s = Snapshot { v: 99, observed_at: 1, session_key: None, five_hour: None, seven_day: None, context: None };
    tap::write_snapshot(&dir.0, KEY, &s).unwrap();
    assert!(tap::read_snapshot(&dir.0, KEY).is_none());
}

/// `JSON.stringify(value, null, 2)` + newline, the format Claude Code saves settings in.
fn js_stringify(v: &Value) -> String {
    // serde_json's pretty printer uses 2-space indentation and `": "`, like JSON.stringify.
    let mut s = serde_json::to_string_pretty(v).unwrap();
    s.push('\n');
    s
}
