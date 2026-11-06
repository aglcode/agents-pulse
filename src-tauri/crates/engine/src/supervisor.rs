//! Collector supervision, failure isolation, enable/disable and graceful shutdown
//! (architecture §3.1, §3.2, §9).
//!
//! Each *enabled* collector gets one long-lived worker task with its own
//! cancellation token (a child of the engine token). Disabling cancels just that
//! token; shutdown cancels the parent. Every run's `detect()` + `collect()`
//! happens in a separate child task, so a panic becomes a `JoinError` instead of
//! taking anything else down, and a hang can be timed out and aborted. Batches
//! reach the writer only after `collect()` returns them whole.
//!
//! Collectors are opt-in: nothing runs until the user enables it. The enabled set
//! is persisted in `app_settings` under [`ENABLED_SETTING`].

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentpulse_core::batch::ResolvedBatch;
use agentpulse_core::collector::{Collector, CollectorError, Ctx, Detection, Trigger};
use agentpulse_core::ids::AgentId;
use agentpulse_core::metric::{CollectionError, ErrorCode};
use agentpulse_core::time::now_ms;
use agentpulse_store::{
    cursors_for, get_setting, AdminOp, CollectorState, CollectorStatusUpdate, FreshnessPolicy, Reader,
    StoreError, WriterHandle, WriterThread,
};
use tokio::sync::{broadcast, Notify};
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::bus::{EngineEvent, BUS_CAPACITY};
use crate::resolver::resolve;

/// `app_settings` key holding the JSON array of enabled collector ids.
pub const ENABLED_SETTING: &str = "collectors.enabled";

/// File events within this window collapse into one collector run.
const WATCH_DEBOUNCE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Delay between collector start-ups at launch (index × stagger), so startup isn't a burst.
    pub initial_stagger: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// Interval multiplier while the window is hidden.
    pub hidden_interval_multiplier: u32,
    pub home_dir: Option<PathBuf>,
    pub freshness: FreshnessPolicy,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            initial_stagger: Duration::from_secs(2),
            backoff_base: Duration::from_secs(30),
            backoff_max: Duration::from_secs(30 * 60),
            hidden_interval_multiplier: 4,
            home_dir: None,
            freshness: FreshnessPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    /// All engine tasks finished within the grace period.
    pub finished_within_grace: bool,
    /// Tasks still running after the grace period that were aborted.
    pub aborted_tasks: usize,
    /// The writer drained, checkpointed and stopped cleanly.
    pub writer_closed_cleanly: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("unknown collector {0}")]
    UnknownCollector(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectorInfo {
    pub collector_id: String,
    pub agent: AgentId,
    pub enabled: bool,
}

struct Slot {
    collector: Arc<dyn Collector>,
    notify: Arc<Notify>,
    /// `Some` while a worker is running.
    worker: Option<CancellationToken>,
}

/// Everything a worker needs, cloned per worker.
#[derive(Clone)]
struct Shared {
    writer: WriterHandle,
    reader: Reader,
    bus: broadcast::Sender<EngineEvent>,
    tracker: TaskTracker,
    visible: Arc<AtomicBool>,
    aborts: Arc<Mutex<Vec<AbortHandle>>>,
    config: EngineConfig,
    rt: tokio::runtime::Handle,
}

pub struct Engine {
    shared: Shared,
    writer_thread: Mutex<Option<WriterThread>>,
    cancel: CancellationToken,
    slots: Mutex<BTreeMap<String, Slot>>,
    /// Serializes enable/disable so the persisted set always matches the running set.
    toggle_lock: tokio::sync::Mutex<()>,
}

impl Engine {
    /// Read the persisted enabled set. Missing or malformed → nothing enabled (opt-in).
    pub fn enabled_from_settings(reader: &Reader) -> HashSet<String> {
        reader
            .with_conn_blocking(|c| get_setting(c, ENABLED_SETTING))
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
            .map(|v| v.into_iter().collect())
            .unwrap_or_default()
    }

    /// Register collectors and start workers for the enabled ones on `rt`.
    /// No runtime is created here.
    pub fn start(
        rt: &tokio::runtime::Handle,
        collectors: Vec<Arc<dyn Collector>>,
        enabled: &HashSet<String>,
        writer_thread: WriterThread,
        reader: Reader,
        config: EngineConfig,
    ) -> Self {
        let (bus, _) = broadcast::channel(BUS_CAPACITY);
        let shared = Shared {
            writer: writer_thread.handle(),
            reader,
            bus,
            tracker: TaskTracker::new(),
            visible: Arc::new(AtomicBool::new(true)),
            aborts: Arc::new(Mutex::new(Vec::new())),
            config,
            rt: rt.clone(),
        };
        let cancel = CancellationToken::new();
        let mut slots = BTreeMap::new();
        let mut started = 0u32;
        for collector in collectors {
            let id = collector.id().0;
            let notify = Arc::new(Notify::new());
            let worker = if enabled.contains(&id) {
                let stagger = shared.config.initial_stagger.saturating_mul(started);
                started += 1;
                Some(spawn_worker(&shared, &cancel, collector.clone(), notify.clone(), stagger))
            } else {
                None
            };
            slots.insert(id, Slot { collector, notify, worker });
        }
        Self {
            shared,
            writer_thread: Mutex::new(Some(writer_thread)),
            cancel,
            slots: Mutex::new(slots),
            toggle_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.shared.bus.subscribe()
    }

    /// Publish an event on behalf of a command.
    pub fn publish(&self, event: EngineEvent) {
        let _ = self.shared.bus.send(event);
    }

    pub fn reader(&self) -> &Reader {
        &self.shared.reader
    }

    pub fn writer(&self) -> &WriterHandle {
        &self.shared.writer
    }

    pub fn freshness(&self) -> FreshnessPolicy {
        self.shared.config.freshness
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn collectors(&self) -> Vec<CollectorInfo> {
        self.slots
            .lock()
            .map(|slots| {
                slots
                    .iter()
                    .map(|(id, s)| CollectorInfo {
                        collector_id: id.clone(),
                        agent: s.collector.agent(),
                        enabled: s.worker.is_some(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Start or stop one collector and persist the choice. Returns whether anything changed.
    /// Disabling marks the collector's metrics unavailable (`collector_disabled`) without touching history.
    pub async fn set_enabled(&self, collector_id: &str, enabled: bool) -> Result<bool, EngineError> {
        let _serial = self.toggle_lock.lock().await;
        let (changed, enabled_ids) = {
            let mut slots = self.slots.lock().map_err(|_| EngineError::UnknownCollector(collector_id.into()))?;
            let slot = slots
                .get_mut(collector_id)
                .ok_or_else(|| EngineError::UnknownCollector(collector_id.to_owned()))?;
            let changed = match (enabled, slot.worker.is_some()) {
                (true, false) => {
                    slot.worker = Some(spawn_worker(
                        &self.shared,
                        &self.cancel,
                        slot.collector.clone(),
                        slot.notify.clone(),
                        Duration::ZERO,
                    ));
                    true
                }
                (false, true) => {
                    if let Some(token) = slot.worker.take() {
                        token.cancel();
                    }
                    true
                }
                _ => false,
            };
            let ids: Vec<String> = slots.iter().filter(|(_, s)| s.worker.is_some()).map(|(id, _)| id.clone()).collect();
            (changed, ids)
        };
        if !changed {
            return Ok(false);
        }
        let value_json = serde_json::to_string(&enabled_ids).unwrap_or_else(|_| "[]".into());
        self.shared.writer.admin(AdminOp::SetSetting { key: ENABLED_SETTING.into(), value_json }).await?;
        if !enabled {
            self.shared
                .writer
                .admin(AdminOp::MarkCollectorDisabled { collector_id: collector_id.to_owned() })
                .await?;
        }
        tracing::info!(collector = %collector_id, enabled, "collector toggled");
        self.publish(EngineEvent::CollectorStatusChanged { collector_id: collector_id.to_owned() });
        Ok(true)
    }

    /// Restart a running collector's worker so it re-reads its triggers (e.g. after
    /// its folder list changed). Availability and history are untouched. Returns
    /// false if the collector is disabled (it picks up the change when enabled).
    pub async fn restart(&self, collector_id: &str) -> Result<bool, EngineError> {
        let _serial = self.toggle_lock.lock().await;
        let mut slots = self.slots.lock().map_err(|_| EngineError::UnknownCollector(collector_id.into()))?;
        let slot = slots
            .get_mut(collector_id)
            .ok_or_else(|| EngineError::UnknownCollector(collector_id.to_owned()))?;
        let Some(old) = slot.worker.take() else { return Ok(false) };
        old.cancel();
        slot.worker = Some(spawn_worker(
            &self.shared,
            &self.cancel,
            slot.collector.clone(),
            slot.notify.clone(),
            Duration::ZERO,
        ));
        tracing::info!(collector = %collector_id, "collector restarted");
        Ok(true)
    }

    /// Enable or disable every registered collector. Returns how many changed.
    pub async fn set_all_enabled(&self, enabled: bool) -> Result<usize, EngineError> {
        let ids: Vec<String> = self.collectors().into_iter().map(|c| c.collector_id).collect();
        let mut changed = 0;
        for id in ids {
            if self.set_enabled(&id, enabled).await? {
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// Wake one enabled collector (or all enabled, with `None`) to run now.
    /// Returns false if the id is unknown or that collector is disabled.
    pub fn refresh(&self, collector_id: Option<&str>) -> bool {
        let Ok(slots) = self.slots.lock() else { return false };
        match collector_id {
            Some(id) => match slots.get(id) {
                Some(s) if s.worker.is_some() => {
                    s.notify.notify_one();
                    true
                }
                _ => false,
            },
            None => {
                slots.values().filter(|s| s.worker.is_some()).for_each(|s| s.notify.notify_one());
                true
            }
        }
    }

    pub fn set_visible(&self, visible: bool) {
        self.shared.visible.store(visible, Ordering::Relaxed);
    }

    pub fn is_visible(&self) -> bool {
        self.shared.visible.load(Ordering::Relaxed)
    }

    /// Graceful shutdown (architecture §3.2):
    /// cancel → wait up to `grace` → close writer intake (drain + checkpoint) → abort stragglers.
    /// Once intake is closed, no straggler can write, whether or not its abort takes effect.
    pub async fn shutdown(&self, grace: Duration, writer_timeout: Duration) -> ShutdownReport {
        self.cancel.cancel();
        self.shared.tracker.close();
        let finished_within_grace = tokio::time::timeout(grace, self.shared.tracker.wait()).await.is_ok();

        let writer_thread = self.writer_thread.lock().ok().and_then(|mut g| g.take());
        let writer_closed_cleanly = match writer_thread {
            Some(w) => match w.shutdown(writer_timeout).await {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(error = %e, "writer did not shut down cleanly");
                    false
                }
            },
            None => false,
        };

        let mut aborted_tasks = 0;
        if let Ok(mut handles) = self.shared.aborts.lock() {
            for h in handles.drain(..) {
                if !h.is_finished() {
                    h.abort();
                    aborted_tasks += 1;
                }
            }
        }
        ShutdownReport { finished_within_grace, aborted_tasks, writer_closed_cleanly }
    }
}

fn spawn_worker(
    shared: &Shared,
    engine_cancel: &CancellationToken,
    collector: Arc<dyn Collector>,
    notify: Arc<Notify>,
    stagger: Duration,
) -> CancellationToken {
    let token = engine_cancel.child_token();
    let worker = Worker { collector, shared: shared.clone(), cancel: token.clone(), notify, stagger };
    shared.tracker.spawn_on(worker.run(), &shared.rt);
    token
}

enum RunOutcome {
    Succeeded,
    NotInstalled,
    Failed(CollectionError),
    Cancelled,
}

enum Collected {
    NotInstalled,
    Batches(Vec<agentpulse_core::batch::NormalizedBatch>),
}

struct Worker {
    collector: Arc<dyn Collector>,
    shared: Shared,
    /// Cancelled when this collector is disabled or the engine shuts down.
    cancel: CancellationToken,
    notify: Arc<Notify>,
    stagger: Duration,
}

impl Worker {
    async fn run(self) {
        let id = self.collector.id().0;
        let base_interval = self
            .collector
            .triggers()
            .iter()
            .find_map(|t| match t {
                Trigger::Interval(d) => Some(*d),
                _ => None,
            })
            .unwrap_or(Duration::from_secs(300));

        // Event-driven collection: file changes wake the worker (debounced). The watcher
        // lives exactly as long as this worker, so disabling the collector stops it.
        let _watcher = self.watch(&id);

        tokio::select! {
            _ = self.cancel.cancelled() => return,
            _ = tokio::time::sleep(self.stagger) => {}
            _ = self.notify.notified() => {}
        }

        let mut failures: u32 = 0;
        let mut last_state: Option<CollectorState> = None;
        loop {
            let delay = match self.run_once(&id).await {
                RunOutcome::Cancelled => return,
                RunOutcome::Succeeded => {
                    failures = 0;
                    if last_state != Some(CollectorState::Ok) {
                        self.emit_status(&id);
                    }
                    last_state = Some(CollectorState::Ok);
                    self.interval(base_interval)
                }
                RunOutcome::NotInstalled => {
                    failures = 0;
                    if last_state != Some(CollectorState::NotInstalled) {
                        self.write_status(&id, CollectorState::NotInstalled, None, None).await;
                    }
                    last_state = Some(CollectorState::NotInstalled);
                    self.interval(base_interval)
                }
                RunOutcome::Failed(err) => {
                    failures = failures.saturating_add(1);
                    let delay = self.backoff(failures);
                    tracing::warn!(collector = %id, code = err.code.as_str(), failures, "collector run failed");
                    let next = now_ms() + i64::try_from(delay.as_millis()).unwrap_or(i64::MAX);
                    self.write_status(&id, CollectorState::Failed, Some(err), Some(next)).await;
                    last_state = Some(CollectorState::Failed);
                    delay
                }
            };
            tokio::select! {
                _ = self.cancel.cancelled() => return,
                _ = tokio::time::sleep(delay) => {}
                _ = self.notify.notified() => {}
            }
        }
    }

    async fn run_once(&self, id: &str) -> RunOutcome {
        let cursors = {
            let id = id.to_owned();
            match self.shared.reader.with_conn(move |c| cursors_for(c, &id)).await {
                Ok(c) => c,
                Err(_) => return RunOutcome::Failed(error(ErrorCode::Storage, "could not read cursors")),
            }
        };
        let mut ctx = Ctx { home_dir: self.shared.config.home_dir.clone(), cursors, installs: vec![] };
        let collector = self.collector.clone();
        let child_cancel = self.cancel.child_token();
        let mut task = self.shared.tracker.spawn_on(
            async move {
                match collector.detect(&ctx).await {
                    Detection::NotInstalled => Ok(Collected::NotInstalled),
                    Detection::Installed(installs) => {
                        ctx.installs = installs;
                        collector.collect(&ctx, &child_cancel).await.map(Collected::Batches)
                    }
                }
            },
            &self.shared.rt,
        );
        self.register_abort(task.abort_handle());

        let joined = tokio::select! {
            _ = self.cancel.cancelled() => {
                task.abort();
                return RunOutcome::Cancelled;
            }
            r = tokio::time::timeout(self.collector.timeout(), &mut task) => r,
        };
        let collected = match joined {
            Err(_) => {
                task.abort();
                return RunOutcome::Failed(error(ErrorCode::Timeout, "collector run timed out"));
            }
            Ok(Err(e)) if e.is_panic() => {
                return RunOutcome::Failed(error(ErrorCode::Panicked, "collector panicked"));
            }
            Ok(Err(_)) | Ok(Ok(Err(CollectorError::Cancelled))) => return RunOutcome::Cancelled,
            Ok(Ok(Err(e))) => return RunOutcome::Failed(error(e.code(), &e.to_string())),
            Ok(Ok(Ok(c))) => c,
        };

        let batches = match collected {
            Collected::NotInstalled => return RunOutcome::NotInstalled,
            Collected::Batches(b) => b,
        };
        for batch in batches {
            if batch.collector_id.as_str() != id || batch.agent != self.collector.agent() {
                return RunOutcome::Failed(error(ErrorCode::InvalidBatch, "batch identity does not match collector"));
            }
            if let Err(e) = batch.validate() {
                return RunOutcome::Failed(error(ErrorCode::InvalidBatch, &e.to_string()));
            }
            let account = match resolve(&batch) {
                Ok(a) => a,
                Err(e) => return RunOutcome::Failed(error(ErrorCode::InvalidBatch, &e.to_string())),
            };
            let result = tokio::select! {
                _ = self.cancel.cancelled() => return RunOutcome::Cancelled,
                r = self.shared.writer.submit(ResolvedBatch { batch, account }) => r,
            };
            match result {
                Ok(summary) => {
                    if summary.changed_data() {
                        let _ = self.shared.bus.send(EngineEvent::DataChanged {
                            collector_id: id.to_owned(),
                            usage_account_id: summary.usage_account_id,
                        });
                    }
                }
                Err(StoreError::WriterClosed) => return RunOutcome::Cancelled,
                Err(e) => return RunOutcome::Failed(error(ErrorCode::Storage, &e.to_string())),
            }
        }
        RunOutcome::Succeeded
    }

    /// Start a debounced recursive watcher on the collector's `WatchPaths`, if any.
    /// Failures are logged and fall back to interval polling.
    fn watch(&self, id: &str) -> Option<notify_debouncer_mini::Debouncer<notify_debouncer_mini::notify::RecommendedWatcher>> {
        use notify_debouncer_mini::notify::RecursiveMode;
        let paths: Vec<PathBuf> = self
            .collector
            .triggers()
            .into_iter()
            .filter_map(|t| match t {
                Trigger::WatchPaths(p) => Some(p),
                _ => None,
            })
            .flatten()
            .filter(|p| p.is_dir())
            .collect();
        if paths.is_empty() {
            return None;
        }
        let notify = self.notify.clone();
        let mut debouncer = match notify_debouncer_mini::new_debouncer(
            WATCH_DEBOUNCE,
            move |res: notify_debouncer_mini::DebounceEventResult| {
                if res.is_ok() {
                    notify.notify_one();
                }
            },
        ) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(collector = %id, error = %e, "file watcher unavailable; using interval polling");
                return None;
            }
        };
        let mut watched = 0;
        for p in &paths {
            match debouncer.watcher().watch(p, RecursiveMode::Recursive) {
                Ok(()) => watched += 1,
                Err(e) => tracing::warn!(collector = %id, error = %e, "could not watch a collector path"),
            }
        }
        tracing::debug!(collector = %id, watched, "file watcher started");
        (watched > 0).then_some(debouncer)
    }

    fn register_abort(&self, handle: AbortHandle) {
        if let Ok(mut v) = self.shared.aborts.lock() {
            v.retain(|h| !h.is_finished());
            v.push(handle);
        }
    }

    async fn write_status(
        &self,
        id: &str,
        state: CollectorState,
        error: Option<CollectionError>,
        next_run_at: Option<i64>,
    ) {
        // A disabled (or shutting down) worker must not overwrite the "disabled" status.
        if self.cancel.is_cancelled() {
            return;
        }
        let update = CollectorStatusUpdate {
            collector_id: id.to_owned(),
            state,
            error,
            next_run_at,
            detected_version: None,
        };
        match self.shared.writer.update_status(update).await {
            Ok(()) => self.emit_status(id),
            Err(StoreError::WriterClosed) => {}
            Err(e) => tracing::warn!(collector = %id, error = %e, "could not record collector status"),
        }
    }

    fn emit_status(&self, id: &str) {
        let _ = self.shared.bus.send(EngineEvent::CollectorStatusChanged { collector_id: id.to_owned() });
    }

    fn interval(&self, base: Duration) -> Duration {
        if self.shared.visible.load(Ordering::Relaxed) {
            base
        } else {
            base.saturating_mul(self.shared.config.hidden_interval_multiplier.max(1))
        }
    }

    /// Exponential backoff with ±10% jitter, capped.
    fn backoff(&self, failures: u32) -> Duration {
        let cfg = &self.shared.config;
        let exp = 2u32.saturating_pow(failures.saturating_sub(1).min(16));
        let base = cfg.backoff_base.saturating_mul(exp).min(cfg.backoff_max);
        let jitter = 0.9 + (now_ms().rem_euclid(1000) as f64 / 1000.0) * 0.2;
        base.mul_f64(jitter).min(cfg.backoff_max)
    }
}

fn error(code: ErrorCode, message: &str) -> CollectionError {
    CollectionError { code, at: now_ms(), message: message.to_owned() }
}
