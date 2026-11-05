//! Narrow, privacy-preserving parser for Codex rollout lines (docs/providers/codex.md).
//!
//! Each line is first read as an envelope whose `payload` stays a borrowed raw JSON
//! slice. Only five line types are ever deserialized further, into structs that
//! name usage fields only. Content-bearing lines (`response_item`, `compacted`,
//! `world_state`, message events) are never deserialized, so prompts, code and
//! tool output are never allocated as values we keep.

use std::borrow::Cow;

use agentpulse_core::batch::{ContextObservation, QuotaObservation, UsageEvent};
use agentpulse_core::hash::stable_hash;
use agentpulse_core::metric::{SourceKind, Stability};
use agentpulse_core::time::{UnixMs, parse_rfc3339_ms};
use agentpulse_core::usage::TokenUsage;
use serde::Deserialize;
use serde_json::value::RawValue;

/// First CLI version that writes per-response `token_usage_record` lines.
const RECORDS_SINCE: (u32, u32, u32) = (0, 153, 0);

const SOURCE: SourceKind = SourceKind::Local;
const STABILITY: Stability = Stability::Undocumented;

#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    ordinal: Option<i64>,
    #[serde(borrow)]
    payload: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct SessionMeta {
    id: Option<String>,
    session_id: Option<String>,
    cli_version: Option<String>,
    timestamp: Option<String>,
}

#[derive(Deserialize)]
struct TurnContext {
    model: Option<String>,
}

#[derive(Deserialize)]
struct EventMsg {
    #[serde(rename = "type")]
    kind: Option<String>,
    info: Option<TokenInfo>,
    rate_limits: Option<RateLimits>,
}

#[derive(Deserialize)]
struct TokenInfo {
    total_token_usage: Option<Usage>,
    last_token_usage: Option<Usage>,
    model_context_window: Option<u64>,
}

#[derive(Deserialize, Clone, Copy, Default, PartialEq, Eq)]
struct Usage {
    input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    cache_write_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_output_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct RateLimits {
    limit_id: Option<String>,
    primary: Option<Window>,
    secondary: Option<Window>,
    plan_type: Option<String>,
}

#[derive(Deserialize)]
struct Window {
    used_percent: Option<f64>,
    window_minutes: Option<u64>,
    resets_at: Option<i64>,
}

#[derive(Deserialize)]
struct Record {
    response_id: Option<String>,
    usage: Option<Usage>,
}

impl Usage {
    /// Codex/OpenAI `input_tokens` includes cached input; AgentPulse `input` is fresh input only.
    fn to_tokens(self) -> TokenUsage {
        let input = match (self.input_tokens, self.cached_input_tokens) {
            (Some(i), Some(c)) => Some(i.saturating_sub(c)),
            (i, _) => i,
        };
        TokenUsage {
            input,
            output: self.output_tokens,
            cache_read: self.cached_input_tokens,
            cache_write: self.cache_write_input_tokens,
            reasoning: self.reasoning_output_tokens,
        }
    }

    /// Per-field difference `self - prev`, for cumulative totals. `None` if any field went backwards.
    fn minus(self, prev: Usage) -> Option<Usage> {
        fn d(a: Option<u64>, b: Option<u64>) -> Option<Option<u64>> {
            match (a, b) {
                (Some(a), Some(b)) => a.checked_sub(b).map(Some),
                (a, None) => Some(a),
                (None, Some(_)) => Some(None),
            }
        }
        Some(Usage {
            input_tokens: d(self.input_tokens, prev.input_tokens)?,
            cached_input_tokens: d(self.cached_input_tokens, prev.cached_input_tokens)?,
            cache_write_input_tokens: d(self.cache_write_input_tokens, prev.cache_write_input_tokens)?,
            output_tokens: d(self.output_tokens, prev.output_tokens)?,
            reasoning_output_tokens: d(self.reasoning_output_tokens, prev.reasoning_output_tokens)?,
        })
    }

    fn is_zero(&self) -> bool {
        [self.input_tokens, self.output_tokens, self.cached_input_tokens, self.cache_write_input_tokens]
            .iter()
            .all(|v| v.unwrap_or(0) == 0)
    }
}

/// Everything learned from one rollout file. Events are emitted only for lines that
/// start at or after `emit_from`; earlier lines still update file context
/// (session id, version, current model, cumulative totals).
#[derive(Debug, Default)]
pub(crate) struct FileScan {
    pub session_key: String,
    pub cli_version: Option<String>,
    pub started_at: Option<UnixMs>,
    pub last_activity_at: Option<UnixMs>,
    pub model: Option<String>,
    pub usage: Vec<UsageEvent>,
    pub quotas: Vec<QuotaObservation>,
    pub context: Option<ContextObservation>,
    /// Latest plan type seen anywhere in the file, with its timestamp.
    pub plan: Option<(UnixMs, String)>,
    /// Byte offset just past the last complete line processed.
    pub consumed: usize,
    /// Lines that could not be parsed (counted, never logged).
    pub bad_lines: u32,
    /// Whether any event was emitted (or the file was otherwise new to us).
    pub emitted: bool,
}

/// Scan one decompressed rollout. `complete` means the file is finished
/// (compressed rollouts), so a final line without a newline is processed too.
pub(crate) fn scan(bytes: &[u8], emit_from: usize, complete: bool, fallback_session: &str) -> FileScan {
    let mut out =
        FileScan { session_key: stable_hash(&["codex-session-file", fallback_session]), ..FileScan::default() };
    let mut session_id: Option<String> = None;
    let mut records_mode = true;
    let mut prev_total: Option<Usage> = None;
    let mut pos = 0usize;

    while pos < bytes.len() {
        let (line, next, terminated) = match bytes[pos..].iter().position(|b| *b == b'\n') {
            Some(i) => (&bytes[pos..pos + i], pos + i + 1, true),
            None => (&bytes[pos..], bytes.len(), false),
        };
        if !terminated && !complete {
            break; // partial line still being written
        }
        let emit = pos >= emit_from;
        let line_start = pos;
        pos = next;
        out.consumed = next;
        let line = trim_cr(line);
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(env) = serde_json::from_slice::<Envelope<'_>>(line) else {
            out.bad_lines += 1;
            continue;
        };
        let ts = env.timestamp.as_deref().and_then(parse_rfc3339_ms);
        if let Some(t) = ts {
            out.last_activity_at = Some(out.last_activity_at.map_or(t, |l| l.max(t)));
        }
        let ordinal = env.ordinal.unwrap_or(i64::try_from(line_start).unwrap_or(i64::MAX));
        let Some(payload) = env.payload else { continue };

        match env.kind.as_ref() {
            "session_meta" => {
                let Ok(meta) = serde_json::from_str::<SessionMeta>(payload.get()) else {
                    out.bad_lines += 1;
                    continue;
                };
                if session_id.is_none() {
                    session_id = meta.id.or(meta.session_id);
                    if let Some(id) = &session_id {
                        out.session_key = stable_hash(&["codex-session", id]);
                    }
                }
                if let Some(v) = meta.cli_version {
                    records_mode = parse_version(&v).is_none_or(|ver| ver >= RECORDS_SINCE);
                    out.cli_version = Some(v);
                }
                let started = meta.timestamp.as_deref().and_then(parse_rfc3339_ms).or(ts);
                if out.started_at.is_none() {
                    out.started_at = started;
                }
            }
            "turn_context" => {
                if let Ok(tc) = serde_json::from_str::<TurnContext>(payload.get())
                    && tc.model.is_some()
                {
                    out.model = tc.model;
                }
            }
            "token_usage_record" => {
                let Ok(rec) = serde_json::from_str::<Record>(payload.get()) else {
                    out.bad_lines += 1;
                    continue;
                };
                if !emit {
                    continue;
                }
                let (Some(id), Some(usage), Some(at)) = (rec.response_id, rec.usage, ts) else {
                    out.bad_lines += 1;
                    continue;
                };
                out.usage.push(usage_event(format!("codex:resp:{id}"), ordinal, &out, at, usage));
                out.emitted = true;
            }
            "event_msg" => {
                let Ok(ev) = serde_json::from_str::<EventMsg>(payload.get()) else {
                    continue; // other event_msg shapes are not ours to parse
                };
                if ev.kind.as_deref() != Some("token_count") {
                    continue;
                }
                if let Some(rl) = &ev.rate_limits
                    && let (Some(plan), Some(at)) = (&rl.plan_type, ts)
                    && out.plan.as_ref().is_none_or(|(t, _)| at >= *t)
                {
                    out.plan = Some((at, plan.clone()));
                }
                if let Some(info) = &ev.info {
                    // Pre-0.153 files have no per-response records: derive usage from
                    // cumulative totals so a repeated token_count never double-counts.
                    if !records_mode && let Some(total) = info.total_token_usage {
                        let delta = match prev_total {
                            Some(prev) => total.minus(prev),
                            None => Some(total),
                        };
                        prev_total = Some(total);
                        if let (true, Some(d), Some(at)) = (emit, delta, ts)
                            && !d.is_zero()
                        {
                            let key = format!("codex:{}:tc:{ordinal}", out.session_key);
                            out.usage.push(usage_event(key, ordinal, &out, at, d));
                            out.emitted = true;
                        }
                    }
                    if emit
                        && let (Some(last), Some(window), Some(at)) =
                            (info.last_token_usage, info.model_context_window, ts)
                        && let Some(used) = last.input_tokens
                    {
                        out.context = Some(ContextObservation {
                            session_key: out.session_key.clone(),
                            used_tokens: used,
                            total_tokens: Some(window),
                            total_source: SOURCE,
                            observed_at: at,
                            source: SOURCE,
                            stability: STABILITY,
                        });
                        out.emitted = true;
                    }
                }
                if emit && let (Some(rl), Some(at)) = (ev.rate_limits, ts) {
                    for w in [rl.primary, rl.secondary].into_iter().flatten() {
                        if let Some(q) = quota(rl.limit_id.as_deref(), &w, at) {
                            out.quotas.push(q);
                            out.emitted = true;
                        }
                    }
                }
            }
            _ => {} // content-bearing or irrelevant line types are never deserialized
        }
    }
    out
}

fn usage_event(dedup_key: String, ordinal: i64, scan: &FileScan, at: UnixMs, usage: Usage) -> UsageEvent {
    UsageEvent {
        dedup_key,
        source_seq: ordinal,
        session_key: Some(scan.session_key.clone()),
        model: scan.model.clone(),
        occurred_at: at,
        tokens: usage.to_tokens(),
        source: SOURCE,
        stability: STABILITY,
    }
}

fn quota(limit_id: Option<&str>, w: &Window, at: UnixMs) -> Option<QuotaObservation> {
    let pct = w.used_percent?;
    if !pct.is_finite() || !(0.0..=1000.0).contains(&pct) {
        return None;
    }
    Some(QuotaObservation {
        window_kind: window_kind(limit_id, w.window_minutes?),
        used_pct: pct,
        // Codex reports epoch seconds; accept milliseconds defensively.
        resets_at: w.resets_at.map(|r| if r > 100_000_000_000 { r } else { r * 1000 }),
        observed_at: at,
        source: SOURCE,
        stability: STABILITY,
    })
}

/// `300` → `5h`, `10080` → `7d`. The main limit (`codex`, or none) gets the bare
/// window name; any other limit (e.g. `premium`) is prefixed so it never overwrites it.
pub(crate) fn window_kind(limit_id: Option<&str>, minutes: u64) -> String {
    let base = if minutes > 0 && minutes.is_multiple_of(1440) {
        format!("{}d", minutes / 1440)
    } else if minutes > 0 && minutes.is_multiple_of(60) {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    };
    match limit_id {
        None | Some("codex") | Some("") => base,
        Some(other) => format!("{other}:{base}"),
    }
}

/// `"0.158.0-alpha.2"` → `(0, 158, 0)`.
pub(crate) fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u32>());
    Some((it.next()?.ok()?, it.next()?.ok()?, it.next().unwrap_or(Ok(0)).ok()?))
}

fn trim_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}
