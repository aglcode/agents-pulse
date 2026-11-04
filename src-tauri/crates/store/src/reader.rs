//! Read side: a small pool of read-only connections used from `spawn_blocking`,
//! so queries never run on async worker threads and never wait on the writer (WAL).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::error::{Result, StoreError};
use crate::schema::open_reader;

const MAX_IDLE: usize = 2;

#[derive(Clone)]
pub struct Reader {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    idle: Mutex<Vec<Connection>>,
}

impl Reader {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { inner: Arc::new(Inner { path, idle: Mutex::new(Vec::new()) }) }
    }

    /// Run a read-only query on a pooled connection, off the async worker threads.
    pub async fn with_conn<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.run(f)).await.map_err(|_| StoreError::Panicked)?
    }

    /// Synchronous variant for non-async callers (tests, maintenance).
    pub fn with_conn_blocking<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.inner.run(f)
    }
}

impl Inner {
    fn run<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = match self.idle.lock().ok().and_then(|mut v| v.pop()) {
            Some(c) => c,
            None => open_reader(&self.path)?,
        };
        let out = f(&conn);
        if let Ok(mut idle) = self.idle.lock()
            && idle.len() < MAX_IDLE
        {
            idle.push(conn);
        }
        out
    }
}
