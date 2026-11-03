//! Engine → frontend event bridge.
//!
//! * Coalesces bursts: at most one emit per topic per second.
//! * Muted while the window is hidden; showing the window triggers a resync.
//! * If the bus lags, emits `engine://resync` so the UI refetches everything.
//! * Emit failures are logged and dropped; they can never crash the app.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use agentpulse_core::dto::DataChangedPayload;
use agentpulse_engine::{Engine, EngineEvent};
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast::error::RecvError;

pub const DATA_CHANGED: &str = "engine://data-changed";
pub const COLLECTOR_STATUS: &str = "engine://collector-status";
pub const RESYNC: &str = "engine://resync";
/// Periodic nudge to re-read data so freshness labels age correctly without new data.
/// Sent from Rust (not a frontend timer) because a hidden WebView2 window still
/// reports `document.visibilityState === "visible"`.
pub const FRESHNESS_TICK: &str = "engine://freshness-tick";

const COALESCE_WINDOW: Duration = Duration::from_secs(1);
const FRESHNESS_TICK_EVERY: Duration = Duration::from_secs(60);

pub fn spawn_bridge(app: AppHandle, engine: Arc<Engine>) {
    spawn_freshness_tick(app.clone(), engine.clone());
    let mut rx = engine.subscribe();
    let cancel = engine.cancellation_token();
    tauri::async_runtime::spawn(async move {
        loop {
            // Wait for the first event of a burst.
            let first = tokio::select! {
                _ = cancel.cancelled() => return,
                ev = rx.recv() => ev,
            };
            let mut pending = Pending::default();
            match first {
                Ok(ev) => pending.add(ev),
                Err(RecvError::Lagged(_)) => pending.resync = true,
                Err(RecvError::Closed) => return,
            }
            // Collect the rest of the burst for the coalescing window.
            let deadline = tokio::time::sleep(COALESCE_WINDOW);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = &mut deadline => break,
                    ev = rx.recv() => match ev {
                        Ok(ev) => pending.add(ev),
                        Err(RecvError::Lagged(_)) => pending.resync = true,
                        Err(RecvError::Closed) => return,
                    },
                }
            }
            if engine.is_visible() {
                pending.emit(&app);
            }
        }
    });
}

fn spawn_freshness_tick(app: AppHandle, engine: Arc<Engine>) {
    let cancel = engine.cancellation_token();
    tauri::async_runtime::spawn(async move {
        let mut every = tokio::time::interval(FRESHNESS_TICK_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        every.tick().await; // the first tick fires immediately; skip it
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = every.tick() => {
                    if engine.is_visible() {
                        emit(&app, FRESHNESS_TICK, ());
                    }
                }
            }
        }
    });
}

#[derive(Default)]
struct Pending {
    collectors: BTreeSet<String>,
    accounts: BTreeSet<i64>,
    status_changed: bool,
    resync: bool,
}

impl Pending {
    fn add(&mut self, ev: EngineEvent) {
        match ev {
            EngineEvent::DataChanged { collector_id, usage_account_id } => {
                self.collectors.insert(collector_id);
                self.accounts.insert(usage_account_id);
            }
            EngineEvent::CollectorStatusChanged { collector_id } => {
                self.collectors.insert(collector_id);
                self.status_changed = true;
            }
        }
    }

    fn emit(self, app: &AppHandle) {
        if self.resync {
            emit(app, RESYNC, ());
            return;
        }
        if !self.accounts.is_empty() {
            let payload = DataChangedPayload {
                collector_ids: self.collectors.iter().cloned().collect(),
                usage_account_ids: self.accounts.into_iter().collect(),
            };
            emit(app, DATA_CHANGED, payload);
        }
        if self.status_changed {
            emit(app, COLLECTOR_STATUS, ());
        }
    }
}

pub fn emit<S: serde::Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    if let Err(e) = app.emit_to(crate::tray::MAIN_WINDOW, event, payload) {
        tracing::debug!(event, error = %e, "event not delivered");
    }
}
