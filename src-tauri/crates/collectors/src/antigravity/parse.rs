//! Narrow, privacy-preserving reader for one Antigravity conversation database
//! (`<data folder>/conversations/<id>.db`, docs/providers/antigravity.md).
//!
//! The database is opened **from memory** (never from Antigravity's folder) and
//! only two columns are selected:
//! * `steps.metadata`, a `CortexStepMetadata`: `1 created_at`, `9 model_usage`
//!   (`ModelUsageStats`) and `11 generator_model`. Nothing else in the message is read.
//! * `gen_metadata.data`, a `CortexStepGeneratorMetadata`: only `1 chat_model`, and
//!   inside it `3 model`, `4 usage.1 model`, `19 response_model` and
//!   `21 model_display_name`, to name the models. Its usage is **not** counted:
//!   every generation record duplicates a step (verified 2026-11-08).
//!
//! `steps.step_payload`, the prompts in `gen_metadata` (`system_prompt`,
//! `message_prompts`, `prompt_debug_str`) and every other column are never
//! selected or decoded. The only strings decoded are the two model-name fields,
//! accepted only if they look like a model name, and the response id, which the
//! caller hashes.

use std::collections::HashMap;

use agentpulse_core::time::UnixMs;
use rusqlite::{Connection, MAIN_DB, OpenFlags};

use super::proto::{Malformed, Value, fields};

/// Token counts from one `ModelUsageStats`. Zero-valued fields are omitted on the
/// wire, so absent and 0 are the same here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Tokens {
    /// Fresh input only: cache reads exceed it in most records (verified 2026-11-08).
    pub input: u64,
    /// Includes thinking: it equals thinking + response output in every record.
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub thinking: u64,
}

impl Tokens {
    pub fn sum(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
    }

    /// Prompt size the model saw: fresh input plus cached input read and written.
    pub fn context_used(&self) -> u64 {
        self.input.saturating_add(self.cache_read).saturating_add(self.cache_write)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Usage {
    /// `Model` enum number. Its value names are placeholders, so names come from `gen_metadata`.
    pub model: Option<u64>,
    pub tokens: Tokens,
    /// Opaque provider id; hashed by the caller, never stored in clear.
    pub response_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Step {
    pub idx: i64,
    pub created_at: Option<UnixMs>,
    pub usage: Option<Usage>,
    /// Set on the conversation's own model steps; absent on auxiliary calls.
    pub generator_model: Option<u64>,
}

#[derive(Debug, Default)]
pub(crate) struct Conversation {
    pub steps: Vec<Step>,
    /// Model enum number → name, from this conversation's own generation records.
    pub names: HashMap<u64, String>,
    /// Rows whose metadata wasn't valid protobuf, or had unexpected wire types.
    pub bad_rows: u32,
    /// Rows in a `step_format` this version doesn't know.
    pub unknown_format_rows: u32,
}

#[derive(Debug)]
pub(crate) enum DbOutcome {
    Read(Conversation),
    /// Not a SQLite database, or SQLite couldn't open it.
    Corrupt,
    /// A SQLite database without the tables and columns verified on 2026-11-08.
    UnsupportedSchema,
}

/// `step_format` values verified on IDE 2.5.5 and app 2.0.11.
const KNOWN_STEP_FORMATS: &[i64] = &[0];
const MAX_NAME: usize = 64;

/// Read one conversation from its in-memory image (WAL already applied).
pub(crate) fn read_conversation(image: &[u8]) -> DbOutcome {
    let Ok(conn) = open(image) else {
        return DbOutcome::Corrupt;
    };
    let has_format = match check_schema(&conn) {
        Ok(Some(f)) => f,
        Ok(None) => return DbOutcome::UnsupportedSchema,
        Err(_) => return DbOutcome::Corrupt,
    };
    match read(&conn, has_format) {
        Ok(c) => DbOutcome::Read(c),
        Err(_) => DbOutcome::Corrupt,
    }
}

fn open(image: &[u8]) -> rusqlite::Result<Connection> {
    // A private in-memory database: nothing on disk is created or opened.
    let mut conn = Connection::open_in_memory_with_flags(OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.deserialize_read_exact(MAIN_DB, image, image.len(), true)?;
    conn.pragma_update(None, "query_only", true)?;
    Ok(conn)
}

/// `Some(has step_format column)` when the verified tables and columns exist.
fn check_schema(conn: &Connection) -> rusqlite::Result<Option<bool>> {
    let columns = |table: &str| -> rusqlite::Result<Vec<String>> {
        let mut s = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
        s.query_map([table], |r| r.get(0))?.collect()
    };
    let steps = columns("steps")?;
    let gens = columns("gen_metadata")?;
    let has = |cols: &[String], name: &str| cols.iter().any(|c| c == name);
    if !(has(&steps, "idx") && has(&steps, "metadata") && has(&gens, "data")) {
        return Ok(None);
    }
    Ok(Some(has(&steps, "step_format")))
}

fn read(conn: &Connection, has_format: bool) -> rusqlite::Result<Conversation> {
    let mut out = Conversation::default();
    let sql = if has_format {
        "SELECT idx, metadata, step_format FROM steps ORDER BY idx"
    } else {
        "SELECT idx, metadata, 0 FROM steps ORDER BY idx"
    };
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let idx: i64 = row.get(0)?;
        let format: Option<i64> = row.get(2)?;
        if !KNOWN_STEP_FORMATS.contains(&format.unwrap_or(0)) {
            out.unknown_format_rows += 1;
            continue;
        }
        let Some(blob) = row.get_ref(1)?.as_blob_or_null()? else {
            continue;
        };
        match step(idx, blob) {
            Ok(s) => out.steps.push(s),
            Err(Malformed) => out.bad_rows += 1,
        }
    }

    // Model names: count each (number, name) pair, separately for API ids and display names.
    let mut api: HashMap<u64, HashMap<String, u32>> = HashMap::new();
    let mut display: HashMap<u64, HashMap<String, u32>> = HashMap::new();
    let mut stmt = conn.prepare("SELECT data FROM gen_metadata")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let Some(blob) = row.get_ref(0)?.as_blob_or_null()? else {
            continue;
        };
        match generation_names(blob) {
            Ok(Some(g)) => {
                if let Some(n) = g.api {
                    *api.entry(g.number).or_default().entry(n).or_default() += 1;
                }
                if let Some(n) = g.display {
                    *display.entry(g.number).or_default().entry(n).or_default() += 1;
                }
            }
            Ok(None) => {}
            Err(Malformed) => out.bad_rows += 1,
        }
    }
    // Prefer the API model id: it's the same for one model across conversations,
    // while the display name adds the mode, e.g. "(High)". A display name is used
    // only when no record of this conversation gives the id.
    for (number, names) in display {
        if let Some(name) = most_common(names) {
            out.names.insert(number, name);
        }
    }
    for (number, names) in api {
        if let Some(name) = most_common(names) {
            out.names.insert(number, name);
        }
    }
    Ok(out)
}

/// The most common name; ties go to the alphabetically first, so the choice is stable.
fn most_common(names: HashMap<String, u32>) -> Option<String> {
    names.into_iter().max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0))).map(|(n, _)| n)
}

fn step(idx: i64, blob: &[u8]) -> Result<Step, Malformed> {
    let mut s = Step { idx, created_at: None, usage: None, generator_model: None };
    for f in fields(blob) {
        match f? {
            (1, Value::Len(b)) => s.created_at = timestamp(b)?,
            (9, Value::Len(b)) => s.usage = Some(usage(b)?),
            (11, Value::Varint(v)) => s.generator_model = Some(v),
            // The fields used must have the verified wire types.
            (1 | 9 | 11, _) => return Err(Malformed),
            _ => {}
        }
    }
    Ok(s)
}

/// `google.protobuf.Timestamp` → unix ms. `None` if unset or out of range.
fn timestamp(b: &[u8]) -> Result<Option<UnixMs>, Malformed> {
    let (mut secs, mut nanos) = (0u64, 0u64);
    for f in fields(b) {
        match f? {
            (1, Value::Varint(v)) => secs = v,
            (2, Value::Varint(v)) => nanos = v,
            (1 | 2, _) => return Err(Malformed),
            _ => {}
        }
    }
    let secs = i64::try_from(secs).map_err(|_| Malformed)?;
    if secs <= 0 || nanos >= 1_000_000_000 {
        return Ok(None);
    }
    Ok(secs.checked_mul(1000).and_then(|ms| ms.checked_add(i64::try_from(nanos / 1_000_000).ok()?)))
}

fn usage(b: &[u8]) -> Result<Usage, Malformed> {
    let mut u = Usage { model: None, tokens: Tokens::default(), response_id: None };
    for f in fields(b) {
        match f? {
            (1, Value::Varint(v)) => u.model = Some(v),
            (2, Value::Varint(v)) => u.tokens.input = v,
            (3, Value::Varint(v)) => u.tokens.output = v,
            (4, Value::Varint(v)) => u.tokens.cache_write = v,
            (5, Value::Varint(v)) => u.tokens.cache_read = v,
            (9, Value::Varint(v)) => u.tokens.thinking = v,
            (11, Value::Len(id)) => u.response_id = std::str::from_utf8(id).ok().filter(|s| !s.is_empty()).map(str::to_owned),
            (1..=5 | 9 | 11, _) => return Err(Malformed),
            _ => {}
        }
    }
    Ok(u)
}

/// The model names one generation record gives for its model number.
#[derive(Debug, PartialEq, Eq)]
struct GenerationNames {
    number: u64,
    /// `response_model`, e.g. `claude-opus-4-6-thinking`.
    api: Option<String>,
    /// `model_display_name`, e.g. `Claude Opus 4.6 (Thinking)`.
    display: Option<String>,
}

/// Model names from one generation record, if it has a model number.
fn generation_names(blob: &[u8]) -> Result<Option<GenerationNames>, Malformed> {
    let mut chat = None;
    for f in fields(blob) {
        if let (1, Value::Len(b)) = f? {
            chat = Some(b);
        }
    }
    let Some(chat) = chat else {
        return Ok(None);
    };
    let (mut number, mut usage_number, mut response_model, mut display) = (None, None, None, None);
    for f in fields(chat) {
        match f? {
            (3, Value::Varint(v)) => number = Some(v),
            (4, Value::Len(u)) => {
                for g in fields(u) {
                    if let (1, Value::Varint(v)) = g? {
                        usage_number = Some(v);
                    }
                }
            }
            (19, Value::Len(s)) => response_model = model_name(s),
            (21, Value::Len(s)) => display = model_name(s),
            _ => {}
        }
    }
    Ok(usage_number.or(number).map(|number| GenerationNames { number, api: response_model, display }))
}

/// A short, model-name-shaped string, or `None`.
fn model_name(b: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(b).ok()?;
    let ok = !s.is_empty()
        && s.len() <= MAX_NAME
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || b" ._-()/:+".contains(&c));
    ok.then(|| s.to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::super::proto::encode::Msg;
    use super::*;

    #[test]
    fn step_fields_and_wire_type_checks() {
        let usage = Msg::new().varint(1, 1026).varint(2, 10).varint(3, 7).varint(5, 900).varint(9, 3).str(11, "resp-1").str(7, "msg");
        let ts = Msg::new().varint(1, 1_762_000_000).varint(2, 250_000_000);
        let m = Msg::new().msg(1, &ts).msg(9, &usage).varint(11, 1026).varint(14, 5).str(99, "anything");
        let s = step(4, &m.0).unwrap();
        assert_eq!(s.created_at, Some(1_762_000_000_250));
        assert_eq!(s.generator_model, Some(1026));
        let u = s.usage.unwrap();
        assert_eq!(u.model, Some(1026));
        assert_eq!(u.tokens, Tokens { input: 10, output: 7, cache_write: 0, cache_read: 900, thinking: 3 });
        assert_eq!(u.response_id.as_deref(), Some("resp-1"));
        assert_eq!(u.tokens.context_used(), 910);

        // A used field with the wrong wire type is rejected, not misread.
        assert_eq!(step(0, &Msg::new().varint(9, 1).0), Err(Malformed));
        assert_eq!(step(0, &Msg::new().msg(9, &Msg::new().str(2, "x")).0), Err(Malformed));
        assert_eq!(step(0, &[0xff, 0xff]), Err(Malformed));
    }

    #[test]
    fn model_names_are_validated() {
        assert_eq!(model_name(b"claude-opus-4-6-thinking").as_deref(), Some("claude-opus-4-6-thinking"));
        assert_eq!(model_name(b"Gemini 3.1 Pro (High)").as_deref(), Some("Gemini 3.1 Pro (High)"));
        assert_eq!(model_name(b"line one\nline two"), None);
        assert_eq!(model_name("x".repeat(65).as_bytes()), None);
        assert_eq!(model_name(b""), None);
        assert_eq!(model_name(&[0xff, 0xfe]), None);
    }

    #[test]
    fn generation_records_give_both_names() {
        let chat = |rm: Option<&str>, dn: Option<&str>| {
            let mut c = Msg::new().str(1, "system prompt").varint(3, 1016).msg(4, &Msg::new().varint(1, 1016));
            if let Some(r) = rm {
                c = c.str(19, r);
            }
            if let Some(d) = dn {
                c = c.str(21, d);
            }
            Msg::new().msg(1, &c).varint(2, 3).str(9, "prompt debug")
        };
        let n = |m: Msg| generation_names(&m.0).unwrap();
        let g = |api: Option<&str>, display: Option<&str>| {
            Some(GenerationNames { number: 1016, api: api.map(Into::into), display: display.map(Into::into) })
        };
        assert_eq!(n(chat(Some("gemini-pro-default"), Some("Gemini 3.1 Pro (High)"))), g(Some("gemini-pro-default"), Some("Gemini 3.1 Pro (High)")));
        assert_eq!(n(chat(None, Some("Gemini 3.1 Pro (High)"))), g(None, Some("Gemini 3.1 Pro (High)")));
        assert_eq!(n(chat(None, None)), g(None, None));
        assert_eq!(n(Msg::new().varint(2, 1)), None);
    }
}
