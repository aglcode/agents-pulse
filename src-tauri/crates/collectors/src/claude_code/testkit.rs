//! Synthetic Claude Code config folders for tests. Line shapes mirror what M2
//! observed in real logs (docs/providers/claude_code.md). `content_lines` and
//! `settings_json` seed every content-bearing field with a sentinel, so privacy
//! tests can prove it never leaves the parser.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use serde_json::{Value, json};

#[derive(Default)]
pub struct LogBuilder {
    lines: Vec<Value>,
}

/// Token counts for one assistant line.
#[derive(Clone, Copy, Default)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl LogBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// One assistant line. Claude Code writes one per content block, so tests call
    /// this repeatedly with the same `message_id` and growing `output`.
    pub fn assistant(&mut self, session: &str, message_id: &str, model: &str, ts: &str, t: Tokens) -> &mut Self {
        self.assistant_ext(session, message_id, model, ts, t, false)
    }

    pub fn assistant_ext(
        &mut self,
        session: &str,
        message_id: &str,
        model: &str,
        ts: &str,
        t: Tokens,
        sidechain: bool,
    ) -> &mut Self {
        self.lines.push(json!({
            "parentUuid": "p", "isSidechain": sidechain, "userType": "external",
            "cwd": "C:\\work\\repo", "sessionId": session, "version": "2.1.318", "gitBranch": "main",
            "message": {
                "id": message_id, "type": "message", "role": "assistant", "model": model,
                "content": [{ "type": "text", "text": "ok" }],
                "stop_reason": null, "stop_sequence": null,
                "usage": {
                    "input_tokens": t.input, "output_tokens": t.output,
                    "cache_read_input_tokens": t.cache_read, "cache_creation_input_tokens": t.cache_write,
                    "cache_creation": { "ephemeral_5m_input_tokens": t.cache_write, "ephemeral_1h_input_tokens": 0 },
                    "service_tier": "standard"
                }
            },
            "requestId": format!("req_{message_id}"), "type": "assistant", "uuid": format!("u-{message_id}"),
            "timestamp": ts
        }));
        self
    }

    /// A Claude Code error/placeholder message: model `<synthetic>`, no API call.
    pub fn synthetic(&mut self, session: &str, ts: &str) -> &mut Self {
        self.lines.push(json!({
            "type": "assistant", "sessionId": session, "timestamp": ts, "isSidechain": false,
            "message": { "id": "synthetic-1", "model": "<synthetic>", "role": "assistant",
                         "content": [{ "type": "text", "text": "API Error" }],
                         "usage": { "input_tokens": 0, "output_tokens": 0 } }
        }));
        self
    }

    /// Content-bearing lines with `sentinel` in every field that could hold user content.
    pub fn content_lines(&mut self, session: &str, sentinel: &str, ts: &str) -> &mut Self {
        let cwd = format!("C:\\projects\\{sentinel}");
        self.lines.push(json!({
            "type": "user", "sessionId": session, "timestamp": ts, "cwd": cwd, "gitBranch": sentinel,
            // The bare word "assistant" passes the byte prefilter; the envelope check must reject it.
            "message": { "role": "user", "content": [{ "type": "text", "text": "assistant" },
                                                     { "type": "text", "text": format!("prompt {sentinel}") }] }
        }));
        self.lines.push(json!({
            "type": "user", "sessionId": session, "timestamp": ts,
            "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t1",
                          "content": format!("fn main() {{ {sentinel} }}") }] },
            "toolUseResult": { "stdout": sentinel, "file": { "filePath": format!("{sentinel}.rs"), "content": sentinel } }
        }));
        // An assistant line whose content carries the sentinel (text, thinking, tool input).
        self.lines.push(json!({
            "type": "assistant", "sessionId": session, "timestamp": ts, "cwd": cwd, "isSidechain": false,
            "message": { "id": "msg_content", "role": "assistant", "model": "claude-opus-4-5",
                "content": [
                    { "type": "text", "text": format!("answer {sentinel}") },
                    { "type": "thinking", "thinking": sentinel, "signature": sentinel },
                    { "type": "tool_use", "id": "t1", "name": "Read", "input": { "file_path": format!("{sentinel}.rs") } }
                ],
                "usage": { "input_tokens": 1, "output_tokens": 1 } }
        }));
        self.lines.push(json!({
            "type": "file-history-snapshot", "messageId": "m",
            "snapshot": { "trackedFileBackups": { format!("C:\\src\\{sentinel}.rs"): { "backupFileName": sentinel } } }
        }));
        self.lines.push(json!({ "type": "summary", "summary": sentinel, "leafUuid": "l" }));
        self.lines.push(json!({ "type": "attachment", "sessionId": session, "content": sentinel }));
        self.lines.push(json!({ "type": "last-prompt", "lastPrompt": sentinel }));
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

/// A `.claude.json` with an `oauthAccount`, plus content-bearing fields seeded with `sentinel`.
pub fn settings_json(account_uuid: &str, email: &str, org: &str, org_type: &str, sentinel: &str) -> String {
    json!({
        "numStartups": 12,
        "userID": format!("user-{sentinel}"),
        "projects": { format!("C:/projects/{sentinel}"): { "history": [{ "display": format!("prompt {sentinel}") }] } },
        "oauthAccount": {
            "accountUuid": account_uuid, "emailAddress": email, "organizationUuid": org,
            "organizationType": org_type, "billingType": "stripe_subscription",
            "displayName": format!("name {sentinel}"), "fullName": sentinel, "organizationName": sentinel
        }
    })
    .to_string()
}

/// Write `<dir>/projects/<project>/<session>.jsonl`.
pub fn write_session(config_dir: &Path, project: &str, session: &str, log: &LogBuilder) -> std::path::PathBuf {
    let path = config_dir.join("projects").join(project).join(format!("{session}.jsonl"));
    log.write(&path);
    path
}
