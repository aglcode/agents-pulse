//! Tauri commands. All are `async` so DB reads never run on the main thread;
//! reads go through the store's pooled readers on `spawn_blocking`.

use std::path::{Path, PathBuf};

use agentpulse_collectors::claude_code::{ClaudeCodeCollector, FolderOrigin, COLLECTOR_ID as CLAUDE_ID};
use agentpulse_core::dto::{ClaudeFolderDto, ClaudeFolderOrigin, ClaudeFoldersDto, DashboardDto};
use agentpulse_core::ids::AgentId;
use agentpulse_core::time::now_ms;
use agentpulse_engine::EngineError;
use serde::Serialize;
use tauri::State;

use crate::state::{AppState, CLAUDE_FOLDERS_SETTING};

/// Error returned to the frontend: a stable code plus a sanitized message.
#[derive(Debug, Serialize)]
pub struct CommandError {
    code: &'static str,
    message: String,
}

impl CommandError {
    fn new(code: &'static str, message: &str) -> Self {
        Self { code, message: message.to_owned() }
    }
}

impl From<agentpulse_store::StoreError> for CommandError {
    fn from(e: agentpulse_store::StoreError) -> Self {
        tracing::warn!(error = %e, "command failed");
        Self::new("storage", "Could not access local data.")
    }
}

impl From<EngineError> for CommandError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::UnknownCollector(_) => Self::new("unknown_collector", "No such collector."),
            EngineError::Store(s) => s.into(),
        }
    }
}

/// One card per AI tool, with collector state, accounts and today's per-model usage.
#[tauri::command]
pub async fn get_dashboard(state: State<'_, AppState>, utc_offset_minutes: i32) -> Result<DashboardDto, CommandError> {
    tracing::debug!("command get_dashboard");
    let policy = state.engine.freshness();
    let offset = utc_offset_minutes.clamp(-14 * 60, 14 * 60);
    let now = now_ms();
    let (overview, models, statuses) = state
        .engine
        .reader()
        .with_conn(move |c| {
            Ok((
                agentpulse_store::overview(c, now, offset, policy)?,
                agentpulse_store::models_today(c, now, offset)?,
                agentpulse_store::collector_statuses(c)?,
            ))
        })
        .await?;
    Ok(crate::dashboard::build(now, overview, models, statuses, state.engine.collectors()))
}

/// Enable or disable the collector for one AI tool. Choices are persisted.
#[tauri::command]
pub async fn set_collector_enabled(state: State<'_, AppState>, agent: AgentId, enabled: bool) -> Result<(), CommandError> {
    let ids: Vec<String> = state
        .engine
        .collectors()
        .into_iter()
        .filter(|c| c.agent == agent)
        .map(|c| c.collector_id)
        .collect();
    if ids.is_empty() {
        return Err(CommandError::new("no_collector", "This AI tool has no collector yet."));
    }
    for id in ids {
        state.engine.set_enabled(&id, enabled).await?;
    }
    Ok(())
}

/// Enable or disable every available collector. Returns how many changed.
#[tauri::command]
pub async fn set_all_collectors_enabled(state: State<'_, AppState>, enabled: bool) -> Result<usize, CommandError> {
    Ok(state.engine.set_all_enabled(enabled).await?)
}

#[tauri::command]
pub async fn refresh(state: State<'_, AppState>, collector_id: Option<String>) -> Result<(), CommandError> {
    if state.engine.refresh(collector_id.as_deref()) {
        Ok(())
    } else {
        Err(CommandError::new("not_running", "That collector isn't enabled."))
    }
}

/// The Claude Code config folders AgentPulse reads, plus suggestions.
#[tauri::command]
pub async fn get_claude_folders(state: State<'_, AppState>) -> Result<ClaudeFoldersDto, CommandError> {
    Ok(claude_folders_dto(claude(&state)?))
}

/// Add a Claude Code config folder (e.g. one used with `CLAUDE_CONFIG_DIR`). The
/// folder is only ever read. It must contain Claude Code's `projects` log folder.
#[tauri::command]
pub async fn add_claude_folder(state: State<'_, AppState>, path: String) -> Result<ClaudeFoldersDto, CommandError> {
    let collector = claude(&state)?;
    let path = expand_home(path.trim(), &collector.default_folder());
    if !path.is_absolute() {
        return Err(CommandError::new("invalid_folder", "Enter the folder's full path."));
    }
    if !path.join("projects").is_dir() {
        return Err(CommandError::new(
            "invalid_folder",
            "That folder has no Claude Code session logs (no \"projects\" folder inside).",
        ));
    }
    if collector.is_collected(&path) {
        return Err(CommandError::new("duplicate_folder", "That folder is already being read."));
    }
    let mut added = added_folders(collector);
    added.push(path);
    apply_claude_folders(&state, collector, added).await?;
    Ok(claude_folders_dto(collector))
}

/// Stop reading a folder the user added. Its history stays; nothing in the folder is touched.
#[tauri::command]
pub async fn remove_claude_folder(state: State<'_, AppState>, path: String) -> Result<ClaudeFoldersDto, CommandError> {
    let collector = claude(&state)?;
    let target = PathBuf::from(path);
    let before = added_folders(collector);
    let added: Vec<PathBuf> = before.iter().filter(|p| **p != target).cloned().collect();
    if added.len() == before.len() {
        return Err(CommandError::new("unknown_folder", "Only folders you added can be removed."));
    }
    apply_claude_folders(&state, collector, added).await?;
    Ok(claude_folders_dto(collector))
}

fn claude<'a>(state: &'a State<'_, AppState>) -> Result<&'a ClaudeCodeCollector, CommandError> {
    state
        .claude
        .as_deref()
        .ok_or_else(|| CommandError::new("no_collector", "Claude Code folders are unavailable (home folder unknown)."))
}

fn added_folders(c: &ClaudeCodeCollector) -> Vec<PathBuf> {
    c.folders().into_iter().filter(|f| f.origin == FolderOrigin::Added).map(|f| f.path).collect()
}

/// Persist the added folders, apply them, and restart the worker so file watching follows.
async fn apply_claude_folders(
    state: &State<'_, AppState>,
    c: &ClaudeCodeCollector,
    added: Vec<PathBuf>,
) -> Result<(), CommandError> {
    let value_json = serde_json::to_string(&added).unwrap_or_else(|_| "[]".into());
    state
        .engine
        .writer()
        .admin(agentpulse_store::AdminOp::SetSetting { key: CLAUDE_FOLDERS_SETTING.into(), value_json })
        .await?;
    c.set_added_folders(added);
    state.engine.restart(CLAUDE_ID).await?;
    tracing::info!(folders = c.folders().len(), "claude code folders changed");
    Ok(())
}

fn claude_folders_dto(c: &ClaudeCodeCollector) -> ClaudeFoldersDto {
    ClaudeFoldersDto {
        folders: c
            .folders()
            .into_iter()
            .map(|f| ClaudeFolderDto {
                has_logs: f.has_logs(),
                path: f.path.to_string_lossy().into_owned(),
                origin: match f.origin {
                    FolderOrigin::Default => ClaudeFolderOrigin::Default,
                    FolderOrigin::Environment => ClaudeFolderOrigin::Environment,
                    FolderOrigin::Added => ClaudeFolderOrigin::Added,
                },
            })
            .collect(),
        suggestions: c.suggestions().into_iter().map(|p| p.to_string_lossy().into_owned()).collect(),
    }
}

/// A leading `~/` or `~\` means the user's home (the default folder's parent).
fn expand_home(input: &str, default_folder: &Path) -> PathBuf {
    let rest = input.strip_prefix("~/").or_else(|| input.strip_prefix("~\\"));
    match (rest, default_folder.parent()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(input),
    }
}
