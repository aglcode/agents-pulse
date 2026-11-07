//! SQLite persistence for AgentPulse: one writer thread, pooled readers, migrations.
//! See architecture §5.

mod admin;
mod apply;
mod error;
mod queries;
mod reader;
mod schema;
mod writer;

#[cfg(test)]
mod tests;

use std::path::Path;

pub use admin::{AdminOp, AdminResult};
pub use apply::ApplySummary;
pub use error::{Result, StoreError};
pub use queries::{
    accounts_for_install, collector_statuses, sessions_summary, cursors_for, get_setting, models_today, overview, FreshnessPolicy,
};
pub use reader::Reader;
pub use schema::SCHEMA_VERSION;
pub use writer::{CollectorState, CollectorStatusUpdate, WriterHandle, WriterThread};

/// Open (creating and migrating if needed) the database at `path`.
/// Returns the writer thread and a reader pool.
pub fn open(path: &Path) -> Result<(WriterThread, Reader)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| StoreError::Io(e.kind()))?;
    }
    let mut conn = schema::open_writer(path)?;
    schema::migrate(&mut conn)?;
    schema::seed_registry(&mut conn)?;
    let writer = WriterThread::spawn(conn)?;
    Ok((writer, Reader::new(path.to_path_buf())))
}
