//! Synthetic Gemini CLI homes for tests. Line shapes mirror the field names verified
//! on Gemini CLI 0.40.0 (docs/providers/gemini_cli.md). Every content-bearing field
//! can be seeded with a sentinel, so privacy tests can prove it never leaves the parser.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Token counts for one reply, as Gemini CLI records them (`input` includes `cached`).
#[derive(Clone, Copy, Default)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    pub thoughts: u64,
    pub tool: u64,
}

pub struct LogBuilder {
    lines: Vec<Value>,
}

impl LogBuilder {
    /// A log starting with its header line. `kind` is `"main"` or `"subagent"`.
    pub fn new(session_id: &str, kind: &str, sentinel: &str) -> Self {
        Self {
            lines: vec![json!({
                "sessionId": session_id, "projectHash": format!("hash-{sentinel}"),
                "startTime": "2026-11-04T01:00:00.000Z", "lastUpdated": "2026-11-04T01:00:00.000Z",
                "kind": kind, "directories": [format!("C:/projects/{sentinel}")]
            })],
        }
    }

    /// A user prompt (no usage).
    pub fn user(&mut self, id: &str, ts: &str, text: &str) -> &mut Self {
        self.lines.push(json!({ "id": id, "timestamp": ts, "type": "user", "content": [{ "text": text }] }));
        self
    }

    /// A model reply. Gemini CLI rewrites a reply as it changes, so tests call this
    /// repeatedly with the same `id`.
    pub fn reply(&mut self, id: &str, model: &str, ts: &str, t: Tokens) -> &mut Self {
        self.reply_with(id, model, ts, t, "ok")
    }

    /// A model reply whose text, thoughts and tool calls all carry `text`.
    pub fn reply_with(&mut self, id: &str, model: &str, ts: &str, t: Tokens, text: &str) -> &mut Self {
        self.lines.push(json!({
            "id": id, "timestamp": ts, "type": "gemini", "content": format!("answer {text}"),
            "thoughts": [{ "subject": text, "description": text, "timestamp": ts }],
            "tokens": { "input": t.input, "output": t.output, "cached": t.cached, "thoughts": t.thoughts,
                        "tool": t.tool, "total": t.input + t.output + t.thoughts + t.tool },
            "model": model,
            "toolCalls": [{
                "id": "c1", "name": "read_file", "args": { "file_path": format!("{text}.rs") },
                "result": [{ "functionResponse": { "id": "c1", "name": "read_file",
                             "response": { "output": format!("fn main() {{ {text} }}") } } }],
                "status": "success", "timestamp": ts, "displayName": "ReadFile",
                "description": text, "resultDisplay": text, "renderOutputAsMarkdown": true
            }]
        }));
        self
    }

    /// An info line and a `$set` update line, both carrying `text`.
    pub fn noise(&mut self, ts: &str, text: &str) -> &mut Self {
        self.lines.push(json!({ "id": "info-1", "timestamp": ts, "type": "info", "content": text }));
        self.lines.push(json!({ "$set": { "lastUpdated": ts, "summary": text } }));
        self
    }

    pub fn build(&self) -> String {
        let mut s = self.lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
        s.push('\n');
        s
    }

    pub fn write(&self, path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, self.build()).unwrap();
    }
}

/// `settings.json` with an auth type, and optionally telemetry, plus a content field.
pub fn settings_json(auth_type: &str, telemetry: Option<Value>) -> String {
    let mut v = json!({ "ide": { "hasSeenNudge": true }, "security": { "auth": { "selectedType": auth_type } } });
    if let Some(t) = telemetry {
        v["telemetry"] = t;
    }
    v.to_string()
}

/// `google_accounts.json`.
pub fn accounts_json(active: &str) -> String {
    json!({ "active": active, "old": [] }).to_string()
}

/// Write `<home>/tmp/<project>/chats/<file>.jsonl`.
pub fn write_session(home: &Path, project: &str, file: &str, log: &LogBuilder) -> PathBuf {
    let path = home.join("tmp").join(project).join("chats").join(format!("{file}.jsonl"));
    log.write(&path);
    path
}

/// Write a subagent log under its parent session's folder.
pub fn write_subagent(home: &Path, project: &str, parent: &str, file: &str, log: &LogBuilder) -> PathBuf {
    let path = home.join("tmp").join(project).join("chats").join(parent).join(format!("{file}.jsonl"));
    log.write(&path);
    path
}

/// Files Gemini CLI keeps next to its logs that AgentPulse must never open, each
/// holding `sentinel`. Some are JSONL-looking on purpose, with usage-shaped lines.
pub fn forbidden_files(home: &Path, project: &str, sentinel: &str) {
    let p = home.join("tmp").join(project);
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("logs.json"), json!([{ "message": format!("prompt {sentinel}") }]).to_string()).unwrap();
    std::fs::write(p.join(".project_root"), format!("C:/projects/{sentinel}")).unwrap();
    std::fs::write(home.join("oauth_creds.json"), json!({ "access_token": sentinel, "refresh_token": sentinel }).to_string())
        .unwrap();
    // A usage-shaped log outside `chats/` must not be read.
    let stray = json!({ "id": "stray", "timestamp": "2026-11-04T01:00:00Z", "type": "gemini", "model": sentinel,
                        "tokens": { "input": 1, "output": 1 } });
    std::fs::write(p.join("stray.jsonl"), format!("{stray}\n")).unwrap();
    std::fs::write(home.join("projects.json"), json!({ format!("C:/projects/{sentinel}"): "x" }).to_string()).unwrap();
}
