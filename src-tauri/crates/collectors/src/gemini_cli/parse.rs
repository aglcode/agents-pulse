//! Narrow, privacy-preserving parser for Gemini CLI session logs
//! (`~/.gemini/tmp/<project>/chats/*.jsonl`, docs/providers/gemini_cli.md).
//!
//! Line kinds verified against Gemini CLI 0.40.0 (field names only):
//! * a header line: `{sessionId, projectHash, startTime, lastUpdated, kind, directories?}`;
//! * message lines: `{id, timestamp, type: user|gemini|info, content, model, tokens, thoughts, toolCalls}`.
//!   A message is written again when it changes, so the same `id` repeats and the
//!   later line wins;
//! * `{"$set": {...}}` update lines.
//!
//! Only `sessionId` and `kind` are read from the header, and only
//! `id`/`type`/`timestamp`/`model`/`tokens` from message lines. `content`,
//! `thoughts`, `toolCalls`, `directories` and `projectHash` are never named, so they
//! are skipped without being kept. A byte prefilter drops every line without a
//! `"tokens"` key before it is deserialized at all.

use std::borrow::Cow;

use agentpulse_core::time::{UnixMs, parse_rfc3339_ms};
use agentpulse_core::usage::TokenUsage;
use serde::Deserialize;

use crate::jsonl_tail::LineOutcome;

#[derive(Deserialize)]
struct HeaderLine {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    kind: Option<String>,
}

/// The parts of a session log's header line AgentPulse uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Header {
    /// Raw session id (hashed by the caller; never stored in clear).
    pub session_id: String,
    /// `kind: "subagent"`: a subagent's own log, stored under its parent session's folder.
    pub subagent: bool,
}

/// Parse a header line. `None` if it isn't one.
pub(crate) fn parse_header(line: &[u8]) -> Option<Header> {
    let h: HeaderLine = serde_json::from_slice(line).ok()?;
    let session_id = h.session_id.filter(|s| !s.is_empty())?;
    Some(Header { session_id, subagent: h.kind.as_deref() == Some("subagent") })
}

#[derive(Deserialize)]
struct MessageLine<'a> {
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
    tokens: Option<Tokens>,
}

/// Gemini API usage metadata as Gemini CLI records it: `input` is the prompt
/// (`promptTokenCount`, cached tokens included), `output` excludes thoughts, and
/// `total = input + output + thoughts + tool` (checked on real lines).
#[derive(Deserialize, Clone, Copy)]
struct Tokens {
    input: Option<u64>,
    output: Option<u64>,
    cached: Option<u64>,
    thoughts: Option<u64>,
    tool: Option<u64>,
}

/// One model reply's usage, as read from one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplyLine {
    pub message_id: String,
    pub model: Option<String>,
    pub occurred_at: UnixMs,
    pub tokens: TokenUsage,
    /// The prompt size of this request (`tokens.input`, cached included): the context in use.
    pub context_used: u64,
}

/// Parse one message line (without its newline).
pub(crate) fn parse_line(line: &[u8]) -> LineOutcome<ReplyLine> {
    if memchr::memmem::find(line, b"\"tokens\"").is_none() {
        return LineOutcome::Ignored;
    }
    let Ok(m) = serde_json::from_slice::<MessageLine<'_>>(line) else {
        return LineOutcome::Bad;
    };
    if m.kind.as_deref() != Some("gemini") {
        return LineOutcome::Ignored;
    }
    let (Some(id), Some(t), Some(at)) = (m.id, m.tokens, m.timestamp.as_deref().and_then(parse_rfc3339_ms)) else {
        return LineOutcome::Ignored;
    };
    if id.is_empty() {
        return LineOutcome::Bad;
    }
    LineOutcome::Item(ReplyLine {
        message_id: id.into_owned(),
        model: m.model.map(Cow::into_owned).filter(|s| !s.is_empty()),
        occurred_at: at,
        tokens: to_usage(t),
        context_used: t.input.unwrap_or(0),
    })
}

/// Convert to AgentPulse's token semantics (`core/src/usage.rs`): fresh input
/// excludes cache reads; output includes reasoning.
fn to_usage(t: Tokens) -> TokenUsage {
    let cached = t.cached.unwrap_or(0);
    let input = match (t.input, t.tool) {
        (None, None) => None,
        // Tool-use prompt tokens are billed as input but aren't part of `input`.
        (i, tool) => Some(i.unwrap_or(0).saturating_sub(cached).saturating_add(tool.unwrap_or(0))),
    };
    let output = match (t.output, t.thoughts) {
        (None, None) => None,
        (o, th) => Some(o.unwrap_or(0).saturating_add(th.unwrap_or(0))),
    };
    TokenUsage { input, output, cache_read: t.cached, cache_write: None, reasoning: t.thoughts }
}

/// Context window size for a Gemini model, **Estimated**: Gemini CLI doesn't log
/// the window size, so it comes from Google's published model limits. `None` for
/// a model not in this table; the total is then shown as unavailable, never guessed.
pub(crate) fn estimated_context_total(model: &str) -> Option<u64> {
    const ONE_M: u64 = 1_048_576;
    const TWO_M: u64 = 2_097_152;
    let m = model.strip_prefix("models/").unwrap_or(model);
    let table: &[(&str, u64)] = &[
        ("gemini-1.5-pro", TWO_M),
        ("gemini-1.5-flash", ONE_M),
        ("gemini-2.0-flash", ONE_M),
        ("gemini-2.5-pro", ONE_M),
        ("gemini-2.5-flash", ONE_M),
        ("gemini-3-pro", ONE_M),
        ("gemini-3-flash", ONE_M),
        ("gemini-3.1-pro", ONE_M),
    ];
    table.iter().find(|(prefix, _)| m.starts_with(prefix)).map(|(_, n)| *n)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn item(line: &str) -> ReplyLine {
        match parse_line(line.as_bytes()) {
            LineOutcome::Item(r) => r,
            _ => panic!("not an item"),
        }
    }

    #[test]
    fn tokens_map_to_agentpulse_semantics() {
        let r = item(
            r#"{"id":"m1","timestamp":"2026-04-30T04:30:00.000Z","type":"gemini","content":"x","model":"gemini-2.5-pro","tokens":{"input":1000,"output":50,"cached":800,"thoughts":20,"tool":5,"total":1075}}"#,
        );
        assert_eq!(r.tokens.input, Some(205), "1000 prompt − 800 cached + 5 tool");
        assert_eq!(r.tokens.cache_read, Some(800));
        assert_eq!(r.tokens.cache_write, None);
        assert_eq!(r.tokens.output, Some(70), "output includes thoughts");
        assert_eq!(r.tokens.reasoning, Some(20));
        assert_eq!(r.context_used, 1000);
        assert_eq!(r.model.as_deref(), Some("gemini-2.5-pro"));
    }

    #[test]
    fn cached_above_input_saturates() {
        let r = item(r#"{"id":"m","timestamp":"2026-04-30T04:30:00Z","type":"gemini","tokens":{"input":10,"cached":30,"output":1}}"#);
        assert_eq!(r.tokens.input, Some(0));
    }

    #[test]
    fn non_reply_lines_are_ignored() {
        for line in [
            r#"{"sessionId":"s","projectHash":"h","startTime":"t","lastUpdated":"t","kind":"main"}"#,
            r#"{"$set":{"lastUpdated":"2026-04-30T04:30:00Z"}}"#,
            r#"{"id":"u1","timestamp":"2026-04-30T04:30:00Z","type":"user","content":"how many \"tokens\" left?"}"#,
            r#"{"id":"i1","timestamp":"2026-04-30T04:30:00Z","type":"info","content":"x","tokens":{"input":1}}"#,
            r#"{"id":"g1","timestamp":"2026-04-30T04:30:00Z","type":"gemini","content":"no usage yet"}"#,
        ] {
            assert!(matches!(parse_line(line.as_bytes()), LineOutcome::Ignored), "{line}");
        }
        assert!(matches!(parse_line(br#"{"tokens": oops"#), LineOutcome::Bad));
    }

    #[test]
    fn header_reads_session_id_and_kind_only() {
        let h = parse_header(br#"{"sessionId":"abc","projectHash":"p","kind":"subagent","directories":["C:/x"]}"#).unwrap();
        assert_eq!(h, Header { session_id: "abc".into(), subagent: true });
        assert!(!parse_header(br#"{"sessionId":"abc","kind":"main"}"#).unwrap().subagent);
        assert_eq!(parse_header(br#"{"id":"m","type":"gemini"}"#), None);
    }

    #[test]
    fn context_totals_come_from_the_model_table() {
        assert_eq!(estimated_context_total("gemini-2.5-pro"), Some(1_048_576));
        assert_eq!(estimated_context_total("gemini-2.5-flash-lite"), Some(1_048_576));
        assert_eq!(estimated_context_total("gemini-3-pro-preview"), Some(1_048_576));
        assert_eq!(estimated_context_total("models/gemini-1.5-pro-002"), Some(2_097_152));
        assert_eq!(estimated_context_total("some-new-model"), None);
    }
}
