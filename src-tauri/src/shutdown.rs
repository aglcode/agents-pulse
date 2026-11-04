//! Graceful exit (architecture §3.2). Any exit request (tray Quit, OS logout, ...)
//! is intercepted once, the engine shuts down (cancel → grace → close writer
//! intake + WAL checkpoint → abort stragglers), logs are flushed, then the
//! app exits for real.

use std::sync::atomic::Ordering;
use std::time::Duration;

use tauri::{AppHandle, Manager, RunEvent};

use crate::state::AppState;

const GRACE: Duration = Duration::from_secs(3);
const WRITER_TIMEOUT: Duration = Duration::from_secs(2);

pub fn on_run_event(app: &AppHandle, event: RunEvent) {
    let RunEvent::ExitRequested { api, .. } = event else { return };
    let Some(state) = app.try_state::<AppState>() else { return };
    if state.shutdown_done.load(Ordering::SeqCst) {
        return; // Second pass, after cleanup: let the exit proceed.
    }
    api.prevent_exit();
    if state.shutting_down.swap(true, Ordering::SeqCst) {
        return; // Cleanup already in progress.
    }
    let app = app.clone();
    let engine = state.engine.clone();
    tauri::async_runtime::spawn(async move {
        let report = engine.shutdown(GRACE, WRITER_TIMEOUT).await;
        tracing::info!(
            finished_within_grace = report.finished_within_grace,
            aborted_tasks = report.aborted_tasks,
            writer_closed_cleanly = report.writer_closed_cleanly,
            "engine shut down"
        );
        if let Some(state) = app.try_state::<AppState>() {
            state.shutdown_done.store(true, Ordering::SeqCst);
            if let Ok(mut guard) = state.log_guard.lock() {
                drop(guard.take()); // flush logs
            }
        }
        app.exit(0);
    });
}
