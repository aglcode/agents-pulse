//! The **only** code in AgentPulse that writes to a Claude Code folder: adding or
//! removing AgentPulse's quota tap as the folder's `statusLine` in `settings.json`.
//! It runs only when the user clicks Enable/Disable on the Claude Code card, after
//! seeing the exact change (approved 2026-11-06; docs/providers/claude_code.md).
//!
//! * **Text edits, not re-serialization.** The block is inserted (or removed) as
//!   text, so every other byte of the file (key order, spacing, line endings) stays
//!   as it was. Each edit is checked by parsing the result: it must equal the
//!   original JSON plus (or minus) exactly the `statusLine` key.
//! * **Never replaces a status line the user set up.** Enabling refuses if one exists.
//! * **No lost updates.** The user approves a preview; applying refuses if the file
//!   changed since (e.g. Claude Code saved a `/model` change in between).
//! * Writes go to a temporary file in the same folder, then replace `settings.json`
//!   in one rename, so the file is never half-written.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Marks a status line command as AgentPulse's tap.
pub const TAP_FLAG: &str = "--statusline-tap";
/// Refuse to edit a settings file larger than this.
const MAX_FILE: u64 = 4 * 1024 * 1024;
const KEY: &str = "statusLine";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapState {
    /// No status line configured.
    Off,
    /// AgentPulse's tap is the status line.
    On,
    /// The user has their own status line; AgentPulse leaves it alone.
    OtherStatusLine,
    /// `settings.json` exists but isn't a JSON object (or can't be read).
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("this folder already has its own status line")]
    OtherStatusLine,
    #[error("AgentPulse's quota tap is already on")]
    AlreadyOn,
    #[error("AgentPulse's quota tap isn't on")]
    NotOn,
    #[error("settings.json isn't valid JSON")]
    InvalidJson,
    #[error("settings.json changed since the preview")]
    Changed,
    #[error("the statusLine block was reformatted; remove it by hand")]
    Reformatted,
    #[error("settings.json is too large")]
    TooLarge,
    #[error("i/o error: {0:?}")]
    Io(std::io::ErrorKind),
}

/// A previewed change to one `settings.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub path: PathBuf,
    /// Current content (`None`: the file doesn't exist yet).
    pub before: Option<String>,
    pub after: String,
}

pub fn settings_path(folder: &Path) -> PathBuf {
    folder.join("settings.json")
}

/// Read-only: is the tap on for this folder?
pub fn tap_state(folder: &Path) -> TapState {
    match read(&settings_path(folder)) {
        Ok(None) => TapState::Off,
        Ok(Some(text)) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => match map.get(KEY) {
                None => TapState::Off,
                Some(v) if is_ours(v) => TapState::On,
                Some(_) => TapState::OtherStatusLine,
            },
            _ => TapState::Unreadable,
        },
        Err(_) => TapState::Unreadable,
    }
}

/// Preview adding the tap. `command` is the full status line command.
pub fn plan_enable(folder: &Path, command: &str) -> Result<Edit, EditError> {
    let path = settings_path(folder);
    let before = read(&path)?;
    let text = before.clone().unwrap_or_default();
    let nl = newline(&text);
    let block = block(command, nl);

    let after = if text.trim().is_empty() {
        format!("{{{nl}  {block}{nl}}}{nl}")
    } else {
        let Value::Object(map) = parse(&text)? else { return Err(EditError::InvalidJson) };
        match map.get(KEY) {
            Some(v) if is_ours(v) => return Err(EditError::AlreadyOn),
            Some(_) => return Err(EditError::OtherStatusLine),
            None => {}
        }
        // Insert before the top-level closing brace, after the last member.
        let close = text.trim_end().len() - 1;
        let (head, tail) = (&text[..close], &text[close..]);
        let head = head.trim_end();
        let sep = if head.ends_with('{') { "" } else { "," };
        format!("{head}{sep}{nl}  {block}{nl}{tail}")
    };
    verify(&text, &after, true)?;
    Ok(Edit { path, before, after })
}

/// Preview removing the tap: exactly the block AgentPulse added, nothing else.
pub fn plan_disable(folder: &Path) -> Result<Edit, EditError> {
    let path = settings_path(folder);
    let text = read(&path)?.ok_or(EditError::NotOn)?;
    let Value::Object(map) = parse(&text)? else { return Err(EditError::InvalidJson) };
    let command = match map.get(KEY) {
        Some(v) if is_ours(v) => v.get("command").and_then(Value::as_str).unwrap_or_default().to_owned(),
        Some(_) => return Err(EditError::OtherStatusLine),
        None => return Err(EditError::NotOn),
    };
    let nl = newline(&text);
    // As written (type, command), or with keys sorted (command, type) by a tool that sorts.
    for block in [block(&command, nl), block_sorted(&command, nl)] {
        // Last or middle key, first key, only key.
        let candidates = [
            (format!(",{nl}  {block}"), String::new()),
            (format!("{nl}  {block},"), String::new()),
            (format!("{{{nl}  {block}{nl}}}"), "{}".to_owned()),
        ];
        for (pattern, replacement) in candidates {
            if text.matches(&pattern).count() == 1 {
                let after = text.replacen(&pattern, &replacement, 1);
                if verify(&after, &text, true).is_ok() {
                    return Ok(Edit { path, before: Some(text), after });
                }
            }
        }
    }
    Err(EditError::Reformatted)
}

/// Apply a previewed edit, only if the file is still exactly what was previewed.
pub fn apply(edit: &Edit) -> Result<(), EditError> {
    if read(&edit.path)? != edit.before {
        return Err(EditError::Changed);
    }
    let tmp = edit.path.with_file_name("settings.json.agentpulse-tmp");
    std::fs::write(&tmp, &edit.after).map_err(io)?;
    if let Err(e) = std::fs::rename(&tmp, &edit.path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(io(e));
    }
    Ok(())
}

fn is_ours(v: &Value) -> bool {
    v.get("command").and_then(Value::as_str).is_some_and(|c| c.contains(TAP_FLAG))
}

/// The block as `JSON.stringify(settings, null, 2)` (Claude Code's own format) writes it.
fn block(command: &str, nl: &str) -> String {
    let cmd = serde_json::to_string(command).unwrap_or_else(|_| "\"\"".into());
    format!("\"{KEY}\": {{{nl}    \"type\": \"command\",{nl}    \"command\": {cmd}{nl}  }}")
}

fn block_sorted(command: &str, nl: &str) -> String {
    let cmd = serde_json::to_string(command).unwrap_or_else(|_| "\"\"".into());
    format!("\"{KEY}\": {{{nl}    \"command\": {cmd},{nl}    \"type\": \"command\"{nl}  }}")
}

/// `with` must equal `without` plus exactly the `statusLine` key (and nothing else).
fn verify(without: &str, with: &str, require_ours: bool) -> Result<(), EditError> {
    let a = if without.trim().is_empty() { Value::Object(Default::default()) } else { parse(without)? };
    let Value::Object(mut b) = parse(with)? else { return Err(EditError::InvalidJson) };
    let added = b.remove(KEY).ok_or(EditError::Reformatted)?;
    if (require_ours && !is_ours(&added)) || Value::Object(b) != a {
        return Err(EditError::Reformatted);
    }
    Ok(())
}

fn parse(text: &str) -> Result<Value, EditError> {
    serde_json::from_str(text).map_err(|_| EditError::InvalidJson)
}

fn newline(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

fn read(path: &Path) -> Result<Option<String>, EditError> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
        Ok(m) if m.len() > MAX_FILE => return Err(EditError::TooLarge),
        Ok(_) => {}
    }
    std::fs::read_to_string(path).map(Some).map_err(io)
}

fn io(e: std::io::Error) -> EditError {
    EditError::Io(e.kind())
}
