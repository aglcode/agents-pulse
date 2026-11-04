//! The single SQLite writer thread and its gated intake (architecture §3.2, §5).
//!
//! * One dedicated `std::thread` owns the only write connection.
//! * Each message is applied as one transaction, under `catch_unwind`.
//! * Shutdown: `accepting` is flipped off, a `Shutdown` message is queued, and on
//!   reading it the writer drops its receiver. Anything sent after that fails with
//!   `WriterClosed` and is never applied. Messages queued *before* `Shutdown` are
//!   applied, which is safe because each one is a complete, atomic unit.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use agentpulse_core::batch::ResolvedBatch;
use agentpulse_core::metric::CollectionError;
use agentpulse_core::time::{now_ms, UnixMs};
use rusqlite::{params, Connection};
use tokio::sync::{mpsc, oneshot};

use crate::admin::{apply_admin, AdminOp, AdminResult};
use crate::apply::{apply_batch, ApplySummary};
use crate::error::{Result, StoreError};

const CHANNEL_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectorState {
    Ok,
    Failed,
    NotInstalled,
    NotRunning,
    Disabled,
}

impl CollectorState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::NotInstalled => "not_installed",
            Self::NotRunning => "not_running",
            Self::Disabled => "disabled",
        }
    }
}

/// Status change that is not part of a successful batch (failures, not-installed, ...).
#[derive(Debug, Clone)]
pub struct CollectorStatusUpdate {
    pub collector_id: String,
    pub state: CollectorState,
    pub error: Option<CollectionError>,
    pub next_run_at: Option<UnixMs>,
    pub detected_version: Option<String>,
}

enum WriterMsg {
    Batch { batch: Box<ResolvedBatch>, reply: oneshot::Sender<Result<ApplySummary>> },
    Status { update: CollectorStatusUpdate, reply: oneshot::Sender<Result<()>> },
    Admin { op: AdminOp, reply: oneshot::Sender<Result<AdminResult>> },
    Shutdown { reply: oneshot::Sender<()> },
}

/// Cloneable handle used by the engine and commands to submit writes.
#[derive(Clone)]
pub struct WriterHandle {
    tx: mpsc::Sender<WriterMsg>,
    accepting: Arc<AtomicBool>,
}

impl WriterHandle {
    pub fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::SeqCst)
    }

    async fn request<T>(&self, make: impl FnOnce(oneshot::Sender<Result<T>>) -> WriterMsg) -> Result<T> {
        if !self.is_accepting() {
            return Err(StoreError::WriterClosed);
        }
        let (reply, rx) = oneshot::channel();
        self.tx.send(make(reply)).await.map_err(|_| StoreError::WriterClosed)?;
        // If the writer dropped our message during shutdown, the reply sender is dropped too.
        rx.await.map_err(|_| StoreError::WriterClosed)?
    }

    /// Apply a complete batch in one transaction.
    pub async fn submit(&self, batch: ResolvedBatch) -> Result<ApplySummary> {
        self.request(|reply| WriterMsg::Batch { batch: Box::new(batch), reply }).await
    }

    pub async fn update_status(&self, update: CollectorStatusUpdate) -> Result<()> {
        self.request(|reply| WriterMsg::Status { update, reply }).await
    }

    pub async fn admin(&self, op: AdminOp) -> Result<AdminResult> {
        self.request(|reply| WriterMsg::Admin { op, reply }).await
    }
}

/// Owns the writer thread. Call [`WriterThread::shutdown`] for a clean stop.
pub struct WriterThread {
    handle: WriterHandle,
    join: Option<JoinHandle<()>>,
}

impl WriterThread {
    pub(crate) fn spawn(conn: Connection) -> Result<Self> {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let join = std::thread::Builder::new()
            .name("agentpulse-db-writer".into())
            .spawn(move || run(conn, rx))
            .map_err(|e| StoreError::Io(e.kind()))?;
        Ok(Self { handle: WriterHandle { tx, accepting: Arc::new(AtomicBool::new(true)) }, join: Some(join) })
    }

    pub fn handle(&self) -> WriterHandle {
        self.handle.clone()
    }

    /// Close intake, let the writer finish what was queued before the close,
    /// checkpoint the WAL and stop the thread.
    pub async fn shutdown(mut self, timeout: Duration) -> Result<()> {
        self.handle.accepting.store(false, Ordering::SeqCst);
        let (reply, rx) = oneshot::channel();
        let tx = self.handle.tx.clone();
        let stop = async move {
            tx.send(WriterMsg::Shutdown { reply }).await.map_err(|_| StoreError::WriterClosed)?;
            rx.await.map_err(|_| StoreError::WriterClosed)
        };
        tokio::time::timeout(timeout, stop).await.map_err(|_| StoreError::Timeout)??;
        if let Some(join) = self.join.take() {
            // The writer has already acknowledged; joining is immediate.
            tokio::task::spawn_blocking(move || join.join())
                .await
                .map_err(|_| StoreError::Panicked)?
                .map_err(|_| StoreError::Panicked)?;
        }
        Ok(())
    }
}

fn run(mut conn: Connection, mut rx: mpsc::Receiver<WriterMsg>) {
    let shutdown_reply = loop {
        let Some(msg) = rx.blocking_recv() else { break None };
        match msg {
            WriterMsg::Batch { batch, reply } => {
                let r = guarded(|| apply_batch(&mut conn, &batch));
                if let Err(e) = &r {
                    tracing::warn!(collector = %batch.batch.collector_id, error = %e, "batch rolled back");
                }
                let _ = reply.send(r);
            }
            WriterMsg::Status { update, reply } => {
                let _ = reply.send(guarded(|| apply_status(&conn, &update)));
            }
            WriterMsg::Admin { op, reply } => {
                let _ = reply.send(guarded(|| apply_admin(&mut conn, op)));
            }
            WriterMsg::Shutdown { reply } => break Some(reply),
        }
    };
    // Close intake: queued-after-shutdown messages are dropped unapplied and their senders see WriterClosed.
    rx.close();
    drop(rx);
    if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
        tracing::warn!(error = %e, "wal checkpoint failed");
    }
    if let Err((_, e)) = conn.close() {
        tracing::warn!(error = %e, "closing sqlite connection failed");
    }
    if let Some(reply) = shutdown_reply {
        let _ = reply.send(());
    }
}

/// A panic inside a write rolls back (the open transaction is dropped) and becomes an error.
fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(Err(StoreError::Panicked))
}

fn apply_status(conn: &Connection, u: &CollectorStatusUpdate) -> Result<()> {
    let (code, message, at) = match &u.error {
        Some(e) => (Some(e.code.as_str()), Some(e.message.as_str()), Some(e.at)),
        None => (None, None, None),
    };
    conn.execute(
        "INSERT INTO collector_status(collector_id, state, detected_version, last_error_code, last_error_message,
             last_error_at, consecutive_failures, next_run_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, CASE WHEN ?4 IS NULL THEN 0 ELSE 1 END, ?7, ?8)
         ON CONFLICT(collector_id) DO UPDATE SET
             state = excluded.state,
             detected_version = COALESCE(excluded.detected_version, collector_status.detected_version),
             last_error_code = COALESCE(excluded.last_error_code, collector_status.last_error_code),
             last_error_message = COALESCE(excluded.last_error_message, collector_status.last_error_message),
             last_error_at = COALESCE(excluded.last_error_at, collector_status.last_error_at),
             consecutive_failures = CASE WHEN excluded.last_error_code IS NULL THEN 0
                                         ELSE collector_status.consecutive_failures + 1 END,
             next_run_at = excluded.next_run_at,
             updated_at = excluded.updated_at",
        params![u.collector_id, u.state.as_str(), u.detected_version, code, message, at, u.next_run_at, now_ms()],
    )?;
    Ok(())
}
