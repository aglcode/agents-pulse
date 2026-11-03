//! Connection setup, migrations and registry seeding.

use std::path::Path;
use std::time::Duration;

use agentpulse_core::registry::{AGENTS, PROVIDERS};
use rusqlite::{Connection, OpenFlags};

use crate::error::{Result, StoreError};

/// Ordered migrations; index + 1 is the resulting `PRAGMA user_version`.
const MIGRATIONS: &[&str] = &[include_str!("../migrations/0001_init.sql")];

pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

pub(crate) fn open_writer(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        tracing::warn!(mode = %mode, "sqlite did not switch to WAL");
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

pub(crate) fn open_reader(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "query_only", "ON")?;
    Ok(conn)
}

pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew { found: current, supported: SCHEMA_VERSION });
    }
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = i as i64 + 1;
        if version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
        tracing::info!(version, "applied migration");
    }
    Ok(())
}

/// Upsert providers, agents and each provider's `unknown` model sentinel from the code registry.
pub(crate) fn seed_registry(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    for p in PROVIDERS {
        tx.execute(
            "INSERT INTO providers(id, display_name) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET display_name = excluded.display_name",
            (p.id.as_str(), p.display_name),
        )?;
        tx.execute(
            "INSERT INTO models(provider_id, name) VALUES (?1, 'unknown') ON CONFLICT(provider_id, name) DO NOTHING",
            [p.id.as_str()],
        )?;
    }
    for a in AGENTS {
        tx.execute(
            "INSERT INTO agents(id, provider_id, display_name, default_entitlement_scope) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET provider_id = excluded.provider_id,
                 display_name = excluded.display_name,
                 default_entitlement_scope = excluded.default_entitlement_scope",
            (a.agent.as_str(), a.provider.as_str(), a.display_name, a.default_scope),
        )?;
    }
    tx.commit()?;
    Ok(())
}
