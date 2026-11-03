use agentpulse_core::batch::BatchError;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// rusqlite errors include SQL text but never bound parameter values.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("writer is closed")]
    WriterClosed,
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("invalid batch: {0}")]
    InvalidBatch(#[from] BatchError),
    #[error("invalid request: {0}")]
    Invalid(&'static str),
    #[error("storage task panicked")]
    Panicked,
    #[error("i/o: {0:?}")]
    Io(std::io::ErrorKind),
    #[error("timed out")]
    Timeout,
}

pub type Result<T> = std::result::Result<T, StoreError>;
