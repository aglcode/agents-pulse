//! Tauri commands. All are `async` so DB reads never run on the main thread;
//! reads go through the store's pooled readers on `spawn_blocking`.

use std::path::{Path, PathBuf};

use agentpulse_collectors::claude_code::settings_edit::{self, TapState};
use agentpulse_collectors::claude_code::{tap, ClaudeCodeCollector, ConfigFolder, FolderOrigin, COLLECTOR_ID as CLAUDE_ID};
use agentpulse_core::dto::{
    ClaudeFolderDto, ClaudeFolderOrigin, ClaudeFoldersDto, DashboardDto, ForgetFolderPreviewDto, QuotaTapPreviewDto,
    QuotaTapState, SkipDefaultFolderPreviewDto,
};
use agentpulse_core::ids::AgentId;
use agentpulse_core::time::now_ms;
use agentpulse_engine::EngineError;
use serde::Serialize;
use tauri::State;

use crate::state::{AppState, CLAUDE_FOLDERS_SETTING, CLAUDE_SKIP_DEFAULT_SETTING};

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
    let collectors = state.engine.collectors();
    let mut notes = Vec::new();
    // Read Gemini CLI's settings only once the user has enabled its collector.
    if let Some(g) = &state.gemini
        && collectors.iter().any(|c| c.agent == AgentId::GeminiCli && c.enabled)
    {
        notes.push((AgentId::GeminiCli, crate::dashboard::gemini_telemetry_note(g.telemetry()).to_owned()));
    }
    if collectors.iter().any(|c| c.agent == AgentId::Antigravity && c.enabled) {
        notes.push((AgentId::Antigravity, crate::dashboard::ANTIGRAVITY_NOTE.to_owned()));
    }
    Ok(crate::dashboard::build(now, overview, models, statuses, collectors, &notes))
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
    folders_response(&state, claude(&state)?).await
}

/// Add a Claude Code config folder (e.g. one used with `CLAUDE_CONFIG_DIR`). The
/// folder is only ever read. It must contain Claude Code's `projects` log folder.
/// Adding the default folder after it was turned off turns it back on.
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
    if collector.default_skipped() && collector.is_default_folder(&path) {
        // Adding the default folder back is how it's turned on again on this machine.
        set_default_skipped(&state, collector, false).await?;
        tracing::info!("claude code default folder turned back on");
        return folders_response(&state, collector).await;
    }
    let mut added = added_folders(collector);
    added.push(path);
    apply_claude_folders(&state, collector, added).await?;
    folders_response(&state, collector).await
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
    folders_response(&state, collector).await
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

/// Preview turning the quota tap on or off for one folder. Writes nothing.
#[tauri::command]
pub async fn preview_quota_tap(
    state: State<'_, AppState>,
    folder: String,
    enable: bool,
) -> Result<QuotaTapPreviewDto, CommandError> {
    let collector = claude(&state)?;
    let f = find_folder(collector, &folder)?;
    let edit = plan_tap(collector, &f, enable)?;
    let (removed_or_kept, added_or_kept) = match enable {
        true => (edit.before.clone().unwrap_or_default(), edit.after.clone()),
        false => (edit.after.clone(), edit.before.clone().unwrap_or_default()),
    };
    Ok(QuotaTapPreviewDto {
        folder,
        settings_path: edit.path.to_string_lossy().into_owned(),
        enable,
        block: changed_lines(&removed_or_kept, &added_or_kept),
        before: edit.before,
    })
}

/// Apply a previewed quota tap change: the only write AgentPulse ever makes to a
/// Claude Code folder. `before` is the file content the user saw in the preview;
/// nothing is written if the file has changed since.
#[tauri::command]
pub async fn set_quota_tap(
    state: State<'_, AppState>,
    folder: String,
    enable: bool,
    before: Option<String>,
) -> Result<ClaudeFoldersDto, CommandError> {
    let collector = claude(&state)?;
    let f = find_folder(collector, &folder)?;
    let edit = plan_tap(collector, &f, enable)?;
    if edit.before != before {
        return Err(changed_error());
    }
    settings_edit::apply(&edit).map_err(edit_error)?;
    tracing::info!(enable, "claude code quota tap changed");
    state.engine.refresh(Some(CLAUDE_ID));
    folders_response(&state, collector).await
}

fn find_folder(c: &ClaudeCodeCollector, path: &str) -> Result<ConfigFolder, CommandError> {
    c.folders()
        .into_iter()
        .find(|f| f.path.to_string_lossy() == path)
        .ok_or_else(|| CommandError::new("unknown_folder", "AgentPulse isn't reading that folder."))
}

fn plan_tap(c: &ClaudeCodeCollector, f: &ConfigFolder, enable: bool) -> Result<settings_edit::Edit, CommandError> {
    if !enable {
        return settings_edit::plan_disable(&f.path).map_err(edit_error);
    }
    if !c.plan_supported(f) {
        return Err(CommandError::new(
            "unsupported_plan",
            "AgentPulse supports Claude Code on Pro, Max and Ultra plans only.",
        ));
    }
    let command = tap_command(c, f).ok_or_else(|| {
        CommandError::new(
            "unsupported",
            "This copy of AgentPulse can't be run by Claude Code as a status line (its install path contains a space).",
        )
    })?;
    settings_edit::plan_enable(&f.path, &command).map_err(edit_error)
}

/// The status line command for this AgentPulse executable, if it can be run safely.
fn tap_command(c: &ClaudeCodeCollector, f: &ConfigFolder) -> Option<String> {
    c.tap_dir()?;
    let exe = std::env::current_exe().ok()?;
    tap::command_for(&exe, c.user_home(), &f.key())
}

/// The lines that differ between `a` and `b` (the block added or removed).
fn changed_lines(a: &str, b: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    b[prefix..b.len() - suffix].join("\n")
}

fn changed_error() -> CommandError {
    CommandError::new(
        "changed",
        "settings.json changed since the preview (for example after a /model change). Nothing was written; review the change again.",
    )
}

fn edit_error(e: settings_edit::EditError) -> CommandError {
    use settings_edit::EditError as E;
    match e {
        E::OtherStatusLine => CommandError::new(
            "other_status_line",
            "This folder already has its own status line. AgentPulse won't replace it.",
        ),
        E::AlreadyOn => CommandError::new("already_on", "The quota tap is already on for this folder."),
        E::NotOn => CommandError::new("not_on", "The quota tap isn't on for this folder."),
        E::InvalidJson => CommandError::new("invalid_json", "settings.json isn't valid JSON, so AgentPulse won't edit it."),
        E::Changed => changed_error(),
        E::Reformatted => CommandError::new(
            "reformatted",
            "The statusLine block was edited by hand, so AgentPulse won't guess. Remove it from settings.json yourself.",
        ),
        E::TooLarge => CommandError::new("too_large", "settings.json is unusually large, so AgentPulse won't edit it."),
        E::Io(kind) => {
            tracing::warn!(?kind, "quota tap edit failed");
            CommandError::new("io", "Could not update settings.json. Nothing was changed.")
        }
    }
}

/// The folder list for the UI. A folder that isn't read is hidden once there's
/// nothing left to remove: no data stored for it and no quota tap in its settings.
async fn folders_response(state: &State<'_, AppState>, c: &ClaudeCodeCollector) -> Result<ClaudeFoldersDto, CommandError> {
    let mut dto = claude_folders_dto(c);
    let mut keep = Vec::with_capacity(dto.folders.len());
    for (folder, cfg) in dto.folders.drain(..).zip(c.folders()) {
        if folder.read || folder.skipped_reason.is_none() || folder.quota_tap == QuotaTapState::On {
            keep.push(folder);
            continue;
        }
        let key = cfg.key();
        let stored = state
            .engine
            .reader()
            .with_conn(move |conn| agentpulse_store::accounts_for_install(conn, AgentId::ClaudeCode.as_str(), &key))
            .await?;
        if !stored.is_empty() {
            keep.push(folder);
        }
    }
    dto.folders = keep;
    Ok(dto)
}

fn claude_folders_dto(c: &ClaudeCodeCollector) -> ClaudeFoldersDto {
    ClaudeFoldersDto {
        folders: c
            .folders()
            .into_iter()
            .map(|f| ClaudeFolderDto {
                quota_tap: match settings_edit::tap_state(&f.path) {
                    TapState::On => QuotaTapState::On,
                    TapState::OtherStatusLine => QuotaTapState::OtherStatusLine,
                    TapState::Unreadable => QuotaTapState::Unreadable,
                    TapState::Off if !c.plan_supported(&f) => QuotaTapState::PlanWithoutQuota,
                    TapState::Off if tap_command(c, &f).is_none() => QuotaTapState::Unsupported,
                    TapState::Off => QuotaTapState::Off,
                },
                read: f.has_logs() && c.plan_supported(&f),
                skipped_reason: skipped_reason(c, &f),
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

/// The default folder as a `ConfigFolder`, whether or not it's currently listed.
fn default_config_folder(c: &ClaudeCodeCollector) -> ConfigFolder {
    ConfigFolder { path: c.default_folder(), origin: FolderOrigin::Default }
}

/// Hashed session keys of the default folder's logs (read-only, off the main thread).
async fn default_session_keys(c: &ClaudeCodeCollector) -> Result<Vec<String>, CommandError> {
    let folder = default_config_folder(c);
    tauri::async_runtime::spawn_blocking(move || folder.session_keys())
        .await
        .map_err(|_| CommandError::new("io", "Could not read the folder's session logs."))?
        .map_err(|e| {
            tracing::warn!(error = %e, "listing default folder sessions failed");
            CommandError::new("io", "Could not read the folder's session logs.")
        })
}

/// Preview turning off Claude Code's default folder on this machine: what AgentPulse
/// would delete from its own database. Reads the folder's logs; writes nothing.
#[tauri::command]
pub async fn preview_skip_default_claude_folder(state: State<'_, AppState>) -> Result<SkipDefaultFolderPreviewDto, CommandError> {
    let collector = claude(&state)?;
    let folder = default_config_folder(collector);
    if collector.default_skipped() {
        return Err(CommandError::new("already_skipped", "The default folder is already turned off on this machine."));
    }
    if settings_edit::tap_state(&folder.path) == TapState::On {
        return Err(CommandError::new("tap_on", "Disable quota for this folder first."));
    }
    let keys = default_session_keys(collector).await?;
    let install_key = folder.key();
    let (accounts, (sessions, records, tokens)) = state
        .engine
        .reader()
        .with_conn(move |c| {
            let agent = AgentId::ClaudeCode.as_str();
            Ok((
                agentpulse_store::accounts_for_install(c, agent, &install_key)?,
                agentpulse_store::sessions_summary(c, agent, &keys)?,
            ))
        })
        .await?;
    Ok(SkipDefaultFolderPreviewDto {
        folder: folder.path.to_string_lossy().into_owned(),
        accounts,
        sessions,
        records,
        tokens,
    })
}

/// Turn off Claude Code's default folder (`~/.claude`) on this machine: stop reading
/// and watching it, and delete the data that came from it from AgentPulse's own
/// database. Nothing in the folder is touched. Other machines are unaffected: the
/// choice is stored in this machine's database only.
#[tauri::command]
pub async fn skip_default_claude_folder(state: State<'_, AppState>) -> Result<ClaudeFoldersDto, CommandError> {
    let collector = claude(&state)?;
    let folder = default_config_folder(collector);
    if settings_edit::tap_state(&folder.path) == TapState::On {
        return Err(CommandError::new("tap_on", "Disable quota for this folder first."));
    }
    let keys = default_session_keys(collector).await?;
    let cursor_keys = collector.cursor_keys(&folder);
    // Stop reading first. A cancelled run never submits, and the writer applies
    // operations in order, so nothing from the folder lands after the delete.
    set_default_skipped(&state, collector, true).await?;
    let result = state
        .engine
        .writer()
        .admin(agentpulse_store::AdminOp::ForgetInstall {
            agent_id: AgentId::ClaudeCode.as_str().to_owned(),
            install_key: folder.key(),
            collector_id: CLAUDE_ID.to_owned(),
            cursor_keys,
            session_keys: keys,
        })
        .await?;
    tracing::info!(?result, "claude code default folder turned off on this machine");
    folders_response(&state, collector).await
}

/// Persist the choice, apply it, and restart the worker so file watching follows.
async fn set_default_skipped(state: &State<'_, AppState>, c: &ClaudeCodeCollector, skip: bool) -> Result<(), CommandError> {
    state
        .engine
        .writer()
        .admin(agentpulse_store::AdminOp::SetSetting { key: CLAUDE_SKIP_DEFAULT_SETTING.into(), value_json: skip.to_string() })
        .await?;
    c.set_default_skipped(skip);
    state.engine.restart(CLAUDE_ID).await?;
    Ok(())
}

/// Why a folder isn't read, if it isn't.
fn skipped_reason(c: &ClaudeCodeCollector, f: &ConfigFolder) -> Option<String> {
    if !f.has_logs() || c.plan_supported(f) {
        return None;
    }
    Some(match c.plan_label(f) {
        Some(plan) => format!("{plan} plan"),
        None => "no claude.ai Pro, Max or Ultra login".to_owned(),
    })
}

/// Preview removing a folder that isn't read (e.g. a Team plan) from AgentPulse.
/// Writes nothing.
#[tauri::command]
pub async fn preview_forget_claude_folder(
    state: State<'_, AppState>,
    folder: String,
) -> Result<ForgetFolderPreviewDto, CommandError> {
    let collector = claude(&state)?;
    let f = find_folder(collector, &folder)?;
    forgettable(collector, &f)?;
    let key = f.key();
    let accounts = state
        .engine
        .reader()
        .with_conn(move |c| agentpulse_store::accounts_for_install(c, AgentId::ClaudeCode.as_str(), &key))
        .await?;
    let tap = tap_removal(&f)?;
    Ok(ForgetFolderPreviewDto {
        folder,
        accounts,
        tap_block: tap.as_ref().map(|e| changed_lines(&e.after, e.before.as_deref().unwrap_or_default())),
        settings_path: settings_edit::settings_path(&f.path).to_string_lossy().into_owned(),
        before: tap.and_then(|e| e.before),
    })
}

/// Remove a folder that isn't read from AgentPulse: take AgentPulse's quota tap out of
/// its `settings.json` (if on; only if the file is unchanged since the preview), delete
/// its accounts' stored data from AgentPulse's own database, and stop listing it if
/// the user had added it. Nothing else in the folder is touched.
#[tauri::command]
pub async fn forget_claude_folder(
    state: State<'_, AppState>,
    folder: String,
    before: Option<String>,
) -> Result<ClaudeFoldersDto, CommandError> {
    let collector = claude(&state)?;
    let f = find_folder(collector, &folder)?;
    forgettable(collector, &f)?;
    if let Some(edit) = tap_removal(&f)? {
        if edit.before != before {
            return Err(changed_error());
        }
        settings_edit::apply(&edit).map_err(edit_error)?;
    }
    let result = state
        .engine
        .writer()
        .admin(agentpulse_store::AdminOp::ForgetInstall {
            agent_id: AgentId::ClaudeCode.as_str().to_owned(),
            install_key: f.key(),
            collector_id: CLAUDE_ID.to_owned(),
            cursor_keys: collector.cursor_keys(&f),
            session_keys: Vec::new(),
        })
        .await?;
    tracing::info!(?result, "claude code folder removed from agentpulse");
    if f.origin == FolderOrigin::Added {
        let added = added_folders(collector).into_iter().filter(|p| *p != f.path).collect();
        apply_claude_folders(&state, collector, added).await?;
    }
    folders_response(&state, collector).await
}

/// Only folders that aren't read can be removed (a read folder would just be collected again).
fn forgettable(c: &ClaudeCodeCollector, f: &ConfigFolder) -> Result<(), CommandError> {
    if f.has_logs() && c.plan_supported(f) {
        return Err(CommandError::new("still_read", "This folder is supported and still being read."));
    }
    Ok(())
}

/// The quota tap removal for this folder, if the tap is on.
fn tap_removal(f: &ConfigFolder) -> Result<Option<settings_edit::Edit>, CommandError> {
    match settings_edit::tap_state(&f.path) {
        TapState::On => settings_edit::plan_disable(&f.path).map(Some).map_err(edit_error),
        _ => Ok(None),
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
