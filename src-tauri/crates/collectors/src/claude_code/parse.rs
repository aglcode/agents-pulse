//! Narrow, privacy-preserving parser for Claude Code session JSONL lines
//! (docs/providers/claude_code.md).
//!
//! * A cheap byte prefilter skips every line that isn't an `assistant` line, so
//!   user prompts, tool output, attachments and `file-history-snapshot` lines
//!   (which use file paths as JSON keys) are never deserialized at all.
//! * Assistant lines are read as an envelope whose `message` stays a borrowed raw
//!   slice, then deserialized into a struct that names usage fields only.
//!   `message.content` is never named, so it's skipped without being kept.

use std::borrow::Cow;

use agentpulse_core::time::{UnixMs, parse_rfc3339_ms};
use agentpulse_core::usage::TokenUsage;
use serde::Deserialize;
use serde_json::value::RawValue;

/// Model id Claude Code writes on locally generated (non-API) assistant messages.
const SYNTHETIC_MODEL: &str = "<synthetic>";

#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    #[serde(rename = "sessionId", borrow)]
    session_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(borrow)]
    version: Option<Cow<'a, str>>,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<bool>,
    #[serde(borrow)]
    message: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize, Clone, Copy)]
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    output_tokens_details: Option<OutputDetails>,
}

#[derive(Deserialize, Clone, Copy)]
struct OutputDetails {
    thinking_tokens: Option<u64>,
}

/// One assistant message's usage, as read from one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AssistantLine {
    pub message_id: String,
    /// Raw session id (hashed by the caller; never stored in clear).
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub occurred_at: UnixMs,
    pub version: Option<String>,
    pub sidechain: bool,
    pub tokens: TokenUsage,
    /// `input + cache_creation + cache_read`: the context in use for this request
    /// (the formula Claude Code's docs give for the status line's `used_percentage`).
    pub context_used: u64,
}

pub(crate) enum Line {
    /// Not an assistant line, or an assistant line without API usage. Never deserialized further.
    Ignored,
    Assistant(Box<AssistantLine>),
    /// Looked like an assistant line but couldn't be parsed (counted, never logged).
    Bad,
}

/// Parse one JSONL line (without its newline).
pub(crate) fn parse_line(line: &[u8]) -> Line {
    if !looks_like_assistant(line) {
        return Line::Ignored;
    }
    let Ok(env) = serde_json::from_slice::<Envelope<'_>>(line) else {
        return Line::Bad;
    };
    if env.kind != "assistant" {
        return Line::Ignored;
    }
    let Some(raw) = env.message else { return Line::Ignored };
    let Ok(msg) = serde_json::from_str::<Message>(raw.get()) else {
        return Line::Bad;
    };
    if msg.model.as_deref() == Some(SYNTHETIC_MODEL) {
        return Line::Ignored;
    }
    let (Some(id), Some(usage), Some(at)) = (msg.id, msg.usage, env.timestamp.as_deref().and_then(parse_rfc3339_ms))
    else {
        return Line::Ignored;
    };
    if id.is_empty() {
        return Line::Bad;
    }
    let tokens = TokenUsage {
        // Anthropic's `input_tokens` already excludes cache reads and writes.
        input: usage.input_tokens,
        output: usage.output_tokens,
        cache_read: usage.cache_read_input_tokens,
        cache_write: usage.cache_creation_input_tokens,
        reasoning: usage.output_tokens_details.and_then(|d| d.thinking_tokens),
    };
    let context_used = [usage.input_tokens, usage.cache_creation_input_tokens, usage.cache_read_input_tokens]
        .iter()
        .map(|v| v.unwrap_or(0))
        .fold(0u64, u64::saturating_add);
    Line::Assistant(Box::new(AssistantLine {
        message_id: id,
        session_id: env.session_id.map(Cow::into_owned),
        model: msg.model,
        occurred_at: at,
        version: env.version.map(Cow::into_owned),
        sidechain: env.is_sidechain.unwrap_or(false),
        tokens,
        context_used,
    }))
}

/// Byte prefilter: only lines mentioning `"assistant"` can be assistant lines.
/// False positives (e.g. a prompt containing the word) are rejected by the
/// envelope check; there are no false negatives.
fn looks_like_assistant(line: &[u8]) -> bool {
    memchr::memmem::find(line, b"\"assistant\"").is_some()
}

/// Context window total for a model, **Estimated**: Claude Code doesn't write the
/// window size to its logs. Current Claude models default to 200k tokens; a
/// session already using more than that must be on the 1M-token window.
pub(crate) fn estimated_context_total(used: u64) -> u64 {
    if used > 200_000 { 1_000_000 } else { 200_000 }
}
