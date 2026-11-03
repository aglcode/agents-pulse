//! User-initiated writes: linking, renaming, rollup rebuilds.
//!
//! None of these rewrite usage records, sessions, segments or rollups keyed by
//! `usage_account_id`, so historical attribution is preserved (architecture §5.4).

use agentpulse_core::time::{now_ms, HOUR_MS};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::error::{Result, StoreError};

#[derive(Debug, Clone)]
pub enum AdminOp {
    /// Declare that these usage accounts are the same entitlement. Non-destructive.
    LinkAccounts { usage_account_ids: Vec<i64>, label: String },
    /// End this account's active link membership. The group dissolves below two members.
    UnlinkAccount { usage_account_id: i64 },
    RenameAccount { usage_account_id: i64, label: String },
    /// Recompute `usage_hourly` from raw `usage_records`.
    RebuildRollups,
    /// Upsert an application setting. `value_json` must be valid JSON.
    SetSetting { key: String, value_json: String },
    /// The user disabled this collector. Its metrics become unavailable with reason
    /// `collector_disabled` (current state only; history is untouched).
    MarkCollectorDisabled { collector_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminResult {
    Linked { group_id: i64 },
    Unlinked,
    Renamed,
    RollupsRebuilt { rows: i64 },
    SettingSaved,
    CollectorMarkedDisabled,
}

pub(crate) fn apply_admin(conn: &mut Connection, op: AdminOp) -> Result<AdminResult> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = now_ms();
    let result = match op {
        AdminOp::LinkAccounts { usage_account_ids, label } => {
            let mut ids = usage_account_ids;
            ids.sort_unstable();
            ids.dedup();
            if ids.len() < 2 {
                return Err(StoreError::Invalid("linking needs at least two accounts"));
            }
            for id in &ids {
                let linked: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM link_memberships WHERE usage_account_id = ?1 AND unlinked_at IS NULL",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if linked.is_some() {
                    return Err(StoreError::Invalid("account is already linked; unlink it first"));
                }
            }
            let group_id: i64 = tx.query_row(
                "INSERT INTO link_groups(display_label, created_at) VALUES (?1, ?2) RETURNING id",
                params![label, now],
                |r| r.get(0),
            )?;
            for id in &ids {
                tx.execute(
                    "INSERT INTO link_memberships(group_id, usage_account_id, linked_at) VALUES (?1, ?2, ?3)",
                    params![group_id, id, now],
                )?;
            }
            AdminResult::Linked { group_id }
        }
        AdminOp::UnlinkAccount { usage_account_id } => {
            let group: Option<i64> = tx
                .query_row(
                    "SELECT group_id FROM link_memberships WHERE usage_account_id = ?1 AND unlinked_at IS NULL",
                    [usage_account_id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(group_id) = group else {
                return Err(StoreError::Invalid("account is not linked"));
            };
            tx.execute(
                "UPDATE link_memberships SET unlinked_at = ?1 WHERE usage_account_id = ?2 AND unlinked_at IS NULL",
                params![now, usage_account_id],
            )?;
            let remaining: i64 = tx.query_row(
                "SELECT COUNT(*) FROM link_memberships WHERE group_id = ?1 AND unlinked_at IS NULL",
                [group_id],
                |r| r.get(0),
            )?;
            if remaining < 2 {
                tx.execute(
                    "UPDATE link_memberships SET unlinked_at = ?1 WHERE group_id = ?2 AND unlinked_at IS NULL",
                    params![now, group_id],
                )?;
            }
            AdminResult::Unlinked
        }
        AdminOp::RenameAccount { usage_account_id, label } => {
            let label = label.trim();
            if label.is_empty() {
                return Err(StoreError::Invalid("label must not be empty"));
            }
            let n = tx.execute(
                "UPDATE usage_accounts SET display_label = ?1, label_user_set = 1 WHERE id = ?2",
                params![label, usage_account_id],
            )?;
            if n == 0 {
                return Err(StoreError::Invalid("no such account"));
            }
            AdminResult::Renamed
        }
        AdminOp::RebuildRollups => {
            tx.execute("DELETE FROM usage_hourly", [])?;
            let rows = tx.execute(REBUILD_HOURLY_SQL, [HOUR_MS])?;
            AdminResult::RollupsRebuilt { rows: i64::try_from(rows).unwrap_or(i64::MAX) }
        }
        AdminOp::SetSetting { key, value_json } => {
            if serde_json::from_str::<serde_json::Value>(&value_json).is_err() {
                return Err(StoreError::Invalid("setting value must be valid JSON"));
            }
            tx.execute(
                "INSERT INTO app_settings(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value_json],
            )?;
            AdminResult::SettingSaved
        }
        AdminOp::MarkCollectorDisabled { collector_id } => {
            tx.execute(
                "UPDATE metric_availability
                 SET state = 'unavailable', source_kind = NULL, stability = NULL,
                     reason = '{\"kind\":\"collector_disabled\"}', since = ?1
                 WHERE collector_id = ?2",
                params![now, collector_id],
            )?;
            tx.execute(
                "INSERT INTO collector_status(collector_id, state, consecutive_failures, next_run_at, updated_at)
                 VALUES (?1, 'disabled', 0, NULL, ?2)
                 ON CONFLICT(collector_id) DO UPDATE SET state = 'disabled', consecutive_failures = 0,
                     next_run_at = NULL, updated_at = excluded.updated_at",
                params![collector_id, now],
            )?;
            AdminResult::CollectorMarkedDisabled
        }
    };
    tx.commit()?;
    Ok(result)
}

/// Full recompute of hourly rollups from raw records (timestamps are positive, so
/// `occurred_at - occurred_at % HOUR` equals the floor used incrementally).
pub(crate) const REBUILD_HOURLY_SQL: &str = "
    INSERT INTO usage_hourly(usage_account_id, agent_id, model_id, hour_utc, input_tokens, output_tokens,
        cache_read_tokens, cache_write_tokens, reasoning_tokens, record_count)
    SELECT usage_account_id, agent_id, model_id, occurred_at - (occurred_at % ?1),
           SUM(IFNULL(input_tokens, 0)), SUM(IFNULL(output_tokens, 0)), SUM(IFNULL(cache_read_tokens, 0)),
           SUM(IFNULL(cache_write_tokens, 0)), SUM(IFNULL(reasoning_tokens, 0)), COUNT(*)
    FROM usage_records
    GROUP BY 1, 2, 3, 4";
