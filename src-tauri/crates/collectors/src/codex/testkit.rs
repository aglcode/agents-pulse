//! Synthetic Codex rollouts for tests. The shapes mirror what M2 observed in real
//! files (docs/providers/codex.md). `content_lines` seeds every content-bearing field
//! with a sentinel, so privacy tests can prove it never leaves the parser.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use serde_json::{Value, json};

/// (limit_id, plan_type, windows as (used_percent, window_minutes, resets_at_secs)).
pub type RateSpec<'a> = (&'a str, &'a str, Vec<(f64, u64, i64)>);

pub struct RolloutBuilder {
    lines: Vec<Value>,
    ordinal: i64,
}

impl Default for RolloutBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RolloutBuilder {
    pub fn new() -> Self {
        Self { lines: Vec::new(), ordinal: 0 }
    }

    fn push(&mut self, kind: &str, ts: &str, payload: Value) -> &mut Self {
        self.ordinal += 1;
        self.lines.push(json!({ "ordinal": self.ordinal, "timestamp": ts, "type": kind, "payload": payload }));
        self
    }

    pub fn session_meta(&mut self, id: &str, cli_version: &str, ts: &str) -> &mut Self {
        self.push(
            "session_meta",
            ts,
            json!({ "id": id, "session_id": id, "cli_version": cli_version, "timestamp": ts,
                    "originator": "codex_cli_rs", "model_provider": "openai" }),
        )
    }

    pub fn turn_context(&mut self, model: &str, ts: &str) -> &mut Self {
        self.push("turn_context", ts, json!({ "model": model, "effort": "medium", "approval_policy": "on-request" }))
    }

    pub fn record(
        &mut self,
        response_id: &str,
        ts: &str,
        input: u64,
        cached: u64,
        output: u64,
        reasoning: u64,
    ) -> &mut Self {
        let usage = json!({ "input_tokens": input, "cached_input_tokens": cached, "cache_write_input_tokens": 0,
                            "output_tokens": output, "reasoning_output_tokens": reasoning, "total_tokens": input + output });
        self.push(
            "token_usage_record",
            ts,
            json!({ "response_id": response_id, "session_id": "s", "thread_id": "t", "turn_id": "u",
                    "usage": usage, "turn_token_usage": usage, "thread_token_usage": usage }),
        )
    }

    /// `totals` / `last`: (input, cached, output). `rate`: (limit_id, plan, [(used%, window_minutes, resets_at_secs)]).
    pub fn token_count(
        &mut self,
        ts: &str,
        totals: Option<(u64, u64, u64)>,
        last: Option<(u64, u64, u64)>,
        window: Option<u64>,
        rate: Option<RateSpec<'_>>,
    ) -> &mut Self {
        let usage = |(i, c, o): (u64, u64, u64)| {
            json!({ "input_tokens": i, "cached_input_tokens": c, "cache_write_input_tokens": 0,
                    "output_tokens": o, "reasoning_output_tokens": 0, "total_tokens": i + o })
        };
        let info = match (totals, last) {
            (None, None) => Value::Null,
            (t, l) => {
                json!({ "total_token_usage": t.map(usage), "last_token_usage": l.map(usage), "model_context_window": window })
            }
        };
        let rate_limits = rate.map(|(limit, plan, windows)| {
            let w = |x: Option<&(f64, u64, i64)>| {
                x.map(|(p, m, r)| json!({ "used_percent": p, "window_minutes": m, "resets_at": r }))
            };
            json!({ "limit_id": limit, "limit_name": null, "primary": w(windows.first()), "secondary": w(windows.get(1)),
                    "credits": { "has_credits": false, "unlimited": false, "balance": "0" },
                    "plan_type": plan, "rate_limit_reached_type": null })
        });
        self.push("event_msg", ts, json!({ "type": "token_count", "info": info, "rate_limits": rate_limits }))
    }

    /// Content-bearing lines with `sentinel` in every field that could hold user content.
    pub fn content_lines(&mut self, sentinel: &str, ts: &str) -> &mut Self {
        self.push(
            "response_item",
            ts,
            json!({ "type": "message", "role": "user",
            "content": [{ "type": "input_text", "text": format!("prompt {sentinel}") }] }),
        );
        self.push(
            "response_item",
            ts,
            json!({ "type": "reasoning", "summary": [{ "text": sentinel }], "encrypted_content": sentinel }),
        );
        self.push(
            "response_item",
            ts,
            json!({ "type": "function_call", "name": "shell",
            "arguments": format!("{{\"command\":[\"cat\",\"{sentinel}.rs\"]}}"), "call_id": "c1" }),
        );
        self.push(
            "response_item",
            ts,
            json!({ "type": "function_call_output", "call_id": "c1", "output": format!("fn main() {{ {sentinel} }}") }),
        );
        self.push("event_msg", ts, json!({ "type": "agent_message", "message": format!("answer {sentinel}") }));
        self.push("event_msg", ts, json!({ "type": "user_message", "message": sentinel }));
        self.push(
            "turn_context",
            ts,
            json!({ "cwd": format!("C:/projects/{sentinel}"), "developer_instructions": sentinel,
            "summary": sentinel, "model": null }),
        );
        self.push("compacted", ts, json!({ "message": sentinel }));
        self.push("world_state", ts, json!({ "files": [sentinel] }))
    }

    pub fn build(&self) -> String {
        let mut s = self.lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
        s.push('\n');
        s
    }

    pub fn write_plain(&self, path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, self.build()).unwrap();
    }

    pub fn write_zst(&self, path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let data = self.build();
        let compressed =
            ruzstd::encoding::compress_to_vec(data.as_bytes(), ruzstd::encoding::CompressionLevel::Fastest);
        std::fs::write(path, compressed).unwrap();
    }
}
