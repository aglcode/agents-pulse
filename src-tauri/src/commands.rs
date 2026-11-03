//! Tauri commands. All are `async` so DB reads never run on the main thread;
//! reads go through the store's pooled readers on `spawn_blocking`.

use agentpulse_core::dto::DashboardDto;
use agentpulse_core::ids::AgentId;
use agentpulse_core::time::now_ms;
use agentpulse_engine::EngineError;
use serde::Serialize;
use tauri::State;

use crate::state::AppState;

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
