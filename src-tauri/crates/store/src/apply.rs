//! Applying one `ResolvedBatch` = one `BEGIN IMMEDIATE` transaction (architecture §5.4).
//!
//! Order: account + binding → sessions → usage (dedup + rollup deltas) → quota and
//! context segments → availability → cursors and collector status → COMMIT.
//! Any error rolls back the whole batch, including its cursor advance.

use std::collections::HashMap;

use agentpulse_core::batch::{Availability, ResolvedBatch, UsageEvent};
use agentpulse_core::identity::AccountResolution;
use agentpulse_core::time::{hour_bucket, UnixMs};
use agentpulse_core::usage::{TokenDelta, TokenUsage};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::error::{Result, StoreError};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplySummary {
    pub usage_account_id: i64,
    pub sessions_upserted: u32,
    pub usage_inserted: u32,
    pub usage_revised: u32,
    pub usage_unchanged: u32,
    pub quota_segments_opened: u32,
    pub quota_segments_extended: u32,
    pub context_segments_opened: u32,
    pub context_segments_extended: u32,
    pub late_observations_dropped: u32,
}

impl ApplySummary {
    /// Whether anything visible to the UI changed.
    pub fn changed_data(&self) -> bool {
        self.sessions_upserted
            + self.usage_inserted
            + self.usage_revised
            + self.quota_segments_opened
            + self.quota_segments_extended
            + self.context_segments_opened
            + self.context_segments_extended
            > 0
    }
}

pub(crate) fn apply_batch(conn: &mut Connection, rb: &ResolvedBatch) -> Result<ApplySummary> {
    rb.batch.validate()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let summary = apply_in_tx(&tx, rb)?;
    tx.commit()?;
    Ok(summary)
}

fn apply_in_tx(tx: &Transaction<'_>, rb: &ResolvedBatch) -> Result<ApplySummary> {
    let b = &rb.batch;
    let now = b.collected_at;
    let agent = b.agent.as_str();
    let mut s = ApplySummary::default();

    let account_id = upsert_account(tx, &rb.account, now)?;
    s.usage_account_id = account_id;
    bind_agent(tx, agent, &b.install_key, account_id, &b.identity.evidence, now)?;

    let mut models = ModelCache::new(rb.account.provider.as_str());
    let mut sessions: HashMap<String, i64> = HashMap::new();

    for obs in &b.sessions {
        let acc = account_at(tx, agent, &b.install_key, obs.started_at, account_id)?;
        let model_id = models.id(tx, obs.model.as_deref())?;
        let unknown = models.id(tx, None)?;
        let id: i64 = tx.query_row(
            "INSERT INTO sessions(usage_account_id, agent_id, external_session_key, model_id,
                                  started_at, last_activity_at, ended_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(agent_id, external_session_key) DO UPDATE SET
                 model_id = CASE WHEN excluded.model_id = ?9 THEN sessions.model_id ELSE excluded.model_id END,
                 started_at = MIN(sessions.started_at, excluded.started_at),
                 last_activity_at = MAX(sessions.last_activity_at, excluded.last_activity_at),
                 ended_at = COALESCE(excluded.ended_at, sessions.ended_at),
                 status = CASE WHEN excluded.last_activity_at >= sessions.last_activity_at
                               THEN excluded.status ELSE sessions.status END
             RETURNING id",
            params![
                acc,
                agent,
                obs.session_key,
                model_id,
                obs.started_at,
                obs.last_activity_at,
                obs.ended_at,
                obs.status.as_str(),
                unknown
            ],
            |r| r.get(0),
        )?;
        sessions.insert(obs.session_key.clone(), id);
        s.sessions_upserted += 1;
    }

    for ev in &b.usage {
        let session_id = match &ev.session_key {
            Some(k) => lookup_session(tx, &sessions, agent, k)?,
            None => None,
        };
        apply_usage(tx, &mut models, &mut s, agent, &b.install_key, account_id, session_id, ev, now)?;
    }

    #[cfg(test)]
    if b.usage.iter().any(|u| u.dedup_key == crate::tests::INJECT_FAILURE_KEY) {
        return Err(StoreError::Invalid("injected failure"));
    }

    for q in &b.quotas {
        let latest: Option<QuotaSegmentRow> = tx
            .query_row(
                "SELECT id, used_pct, resets_at, source_kind, stability, last_observed_at
                 FROM quota_segments WHERE usage_account_id = ?1 AND window_kind = ?2
                 ORDER BY last_observed_at DESC LIMIT 1",
                params![account_id, q.window_kind],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()?;
        let same = |l: &QuotaSegmentRow| {
            (l.1 - q.used_pct).abs() < 1e-9
                && l.2 == q.resets_at
                && l.3 == q.source.as_str()
                && l.4 == q.stability.as_str()
        };
        match segment_action(latest.as_ref().map(|l| (l.5, same(l))), q.observed_at) {
            SegmentAction::DropLate => s.late_observations_dropped += 1,
            SegmentAction::Extend => {
                if let Some(l) = &latest {
                    tx.execute(
                        "UPDATE quota_segments SET last_observed_at = MAX(last_observed_at, ?1) WHERE id = ?2",
                        params![q.observed_at, l.0],
                    )?;
                }
                s.quota_segments_extended += 1;
            }
            SegmentAction::Open => {
                tx.execute(
                    "INSERT INTO quota_segments(usage_account_id, window_kind, used_pct, resets_at,
                         source_kind, stability, first_observed_at, last_observed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                    params![
                        account_id,
                        q.window_kind,
                        q.used_pct,
                        q.resets_at,
                        q.source.as_str(),
                        q.stability.as_str(),
                        q.observed_at
                    ],
                )?;
                s.quota_segments_opened += 1;
            }
        }
    }

    for c in &b.contexts {
        let session_id = match lookup_session(tx, &sessions, agent, &c.session_key)? {
            Some(id) => id,
            None => {
                // Context for a session we haven't seen yet: create a minimal session row.
                let unknown = models.id(tx, None)?;
                let acc = account_at(tx, agent, &b.install_key, c.observed_at, account_id)?;
                tx.query_row(
                    "INSERT INTO sessions(usage_account_id, agent_id, external_session_key, model_id,
                                          started_at, last_activity_at, status)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5, 'unknown') RETURNING id",
                    params![acc, agent, c.session_key, unknown, c.observed_at],
                    |r| r.get(0),
                )?
            }
        };
        let used = to_i64(c.used_tokens);
        let total = c.total_tokens.map(to_i64);
        let latest: Option<ContextSegmentRow> = tx
            .query_row(
                "SELECT id, used_tokens, total_tokens, total_source, source_kind, stability, last_observed_at
                 FROM context_segments WHERE session_id = ?1 ORDER BY last_observed_at DESC LIMIT 1",
                [session_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .optional()?;
        let same = |l: &ContextSegmentRow| {
            l.1 == used
                && l.2 == total
                && l.3 == c.total_source.as_str()
                && l.4 == c.source.as_str()
                && l.5 == c.stability.as_str()
        };
        match segment_action(latest.as_ref().map(|l| (l.6, same(l))), c.observed_at) {
            SegmentAction::DropLate => s.late_observations_dropped += 1,
            SegmentAction::Extend => {
                if let Some(l) = &latest {
                    tx.execute(
                        "UPDATE context_segments SET last_observed_at = MAX(last_observed_at, ?1) WHERE id = ?2",
                        params![c.observed_at, l.0],
                    )?;
                }
                s.context_segments_extended += 1;
            }
            SegmentAction::Open => {
                tx.execute(
                    "INSERT INTO context_segments(session_id, used_tokens, total_tokens, total_source,
                         source_kind, stability, first_observed_at, last_observed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                    params![
                        session_id,
                        used,
                        total,
                        c.total_source.as_str(),
                        c.source.as_str(),
                        c.stability.as_str(),
                        c.observed_at
                    ],
                )?;
                s.context_segments_opened += 1;
            }
        }
    }

    for a in &b.availability {
        let (state, source, stability, reason) = match &a.availability {
            Availability::Available { source, stability } => {
                ("available", Some(source.as_str()), Some(stability.as_str()), None)
            }
            Availability::Unavailable(r) => (
                "unavailable",
                None,
                None,
                Some(serde_json::to_string(r).map_err(|_| StoreError::Invalid("unencodable reason"))?),
            ),
        };
        tx.execute(
            "INSERT INTO metric_availability(collector_id, usage_account_id, metric, state, source_kind, stability, reason, since)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(collector_id, usage_account_id, metric) DO UPDATE SET
                 since = CASE WHEN metric_availability.state = excluded.state
                                   AND IFNULL(metric_availability.reason, '') = IFNULL(excluded.reason, '')
                                   AND IFNULL(metric_availability.source_kind, '') = IFNULL(excluded.source_kind, '')
                              THEN metric_availability.since ELSE excluded.since END,
                 state = excluded.state, source_kind = excluded.source_kind,
                 stability = excluded.stability, reason = excluded.reason",
            params![b.collector_id.as_str(), account_id, a.metric, state, source, stability, reason, now],
        )?;
    }

    for c in &b.cursors {
        tx.execute(
            "INSERT INTO collector_cursors(collector_id, source_key, byte_offset, file_id, size, mtime)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(collector_id, source_key) DO UPDATE SET byte_offset = excluded.byte_offset,
                 file_id = excluded.file_id, size = excluded.size, mtime = excluded.mtime",
            params![
                b.collector_id.as_str(),
                c.source_key,
                to_i64(c.state.byte_offset),
                c.state.file_id,
                to_i64(c.state.size),
                c.state.mtime
            ],
        )?;
    }

    tx.execute(
        "UPDATE usage_accounts SET last_collected_at = MAX(IFNULL(last_collected_at, 0), ?1) WHERE id = ?2",
        params![now, account_id],
    )?;
    tx.execute(
        "INSERT INTO collector_status(collector_id, state, detected_version, last_success_at, consecutive_failures, next_run_at, updated_at)
         VALUES (?1, 'ok', ?2, ?3, 0, NULL, ?3)
         ON CONFLICT(collector_id) DO UPDATE SET state = 'ok',
             detected_version = COALESCE(excluded.detected_version, collector_status.detected_version),
             last_success_at = excluded.last_success_at, consecutive_failures = 0,
             next_run_at = NULL, updated_at = excluded.updated_at",
        params![b.collector_id.as_str(), b.detected_version, now],
    )?;

    Ok(s)
}

/// (id, used_pct, resets_at, source_kind, stability, last_observed_at)
type QuotaSegmentRow = (i64, f64, Option<i64>, String, String, i64);
/// (id, used_tokens, total_tokens, total_source, source_kind, stability, last_observed_at)
type ContextSegmentRow = (i64, i64, Option<i64>, String, String, String, i64);

#[derive(Debug, PartialEq, Eq)]
enum SegmentAction {
    Open,
    Extend,
    DropLate,
}

/// `latest` = (last_observed_at, value_is_same) of the most recent segment for the key.
/// Gauges are point-in-time: a reading older than what we already have must not rewrite history.
fn segment_action(latest: Option<(UnixMs, bool)>, observed_at: UnixMs) -> SegmentAction {
    match latest {
        None => SegmentAction::Open,
        Some((last, _)) if observed_at < last => SegmentAction::DropLate,
        Some((last, same)) if observed_at == last => {
            if same {
                SegmentAction::Extend
            } else {
                SegmentAction::DropLate
            }
        }
        Some((_, true)) => SegmentAction::Extend,
        Some((_, false)) => SegmentAction::Open,
    }
}

fn upsert_account(tx: &Transaction<'_>, acc: &AccountResolution, now: UnixMs) -> Result<i64> {
    let provider = acc.provider.as_str();
    let identity_id: Option<i64> = match &acc.identity {
        Some(id) => Some(tx.query_row(
            "INSERT INTO identities(provider_id, kind, identity_key, display_label, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(provider_id, identity_key) DO UPDATE SET
                 display_label = COALESCE(excluded.display_label, identities.display_label)
             RETURNING id",
            params![provider, id.kind.as_str(), id.identity_key, id.display_label, now],
            |r| r.get(0),
        )?),
        None => None,
    };
    let id = tx.query_row(
        "INSERT INTO usage_accounts(provider_id, entitlement_scope, account_key, identity_id, kind, display_label, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(provider_id, entitlement_scope, account_key) DO UPDATE SET
             identity_id = COALESCE(excluded.identity_id, usage_accounts.identity_id),
             display_label = CASE WHEN usage_accounts.label_user_set = 1
                                  THEN usage_accounts.display_label ELSE excluded.display_label END
         RETURNING id",
        params![
            provider,
            acc.entitlement_scope,
            acc.account_key,
            identity_id,
            acc.account_kind.as_str(),
            acc.default_label,
            now
        ],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// Keep exactly one active binding per (agent, install); close it when the account changes.
fn bind_agent(
    tx: &Transaction<'_>,
    agent: &str,
    install_key: &str,
    account_id: i64,
    evidence: &str,
    now: UnixMs,
) -> Result<()> {
    let active: Option<(i64, i64, i64)> = tx
        .query_row(
            "SELECT id, usage_account_id, bound_at FROM agent_bindings
             WHERE agent_id = ?1 AND install_key = ?2 AND unbound_at IS NULL",
            params![agent, install_key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let bound_at = match active {
        Some((_, acc, _)) if acc == account_id => return Ok(()),
        Some((id, _, prev_bound)) => {
            let at = now.max(prev_bound + 1);
            tx.execute("UPDATE agent_bindings SET unbound_at = ?1 WHERE id = ?2", params![at, id])?;
            at
        }
        None => {
            let last: Option<i64> = tx.query_row(
                "SELECT MAX(bound_at) FROM agent_bindings WHERE agent_id = ?1 AND install_key = ?2",
                params![agent, install_key],
                |r| r.get(0),
            )?;
            last.map_or(now, |l| now.max(l + 1))
        }
    };
    tx.execute(
        "INSERT INTO agent_bindings(agent_id, install_key, usage_account_id, evidence, bound_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![agent, install_key, account_id, evidence, bound_at],
    )?;
    Ok(())
}

/// Usage account that was bound to this installation at time `at`. Before the first
/// binding, the earliest binding applies; with no bindings at all, `current`.
fn account_at(tx: &Transaction<'_>, agent: &str, install_key: &str, at: UnixMs, current: i64) -> Result<i64> {
    let at_time: Option<i64> = tx
        .query_row(
            "SELECT usage_account_id FROM agent_bindings
             WHERE agent_id = ?1 AND install_key = ?2 AND bound_at <= ?3
             ORDER BY bound_at DESC LIMIT 1",
            params![agent, install_key, at],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(a) = at_time {
        return Ok(a);
    }
    let earliest: Option<i64> = tx
        .query_row(
            "SELECT usage_account_id FROM agent_bindings
             WHERE agent_id = ?1 AND install_key = ?2 ORDER BY bound_at ASC LIMIT 1",
            params![agent, install_key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(earliest.unwrap_or(current))
}

fn lookup_session(
    tx: &Transaction<'_>,
    cache: &HashMap<String, i64>,
    agent: &str,
    key: &str,
) -> Result<Option<i64>> {
    if let Some(id) = cache.get(key) {
        return Ok(Some(*id));
    }
    Ok(tx
        .query_row(
            "SELECT id FROM sessions WHERE agent_id = ?1 AND external_session_key = ?2",
            params![agent, key],
            |r| r.get(0),
        )
        .optional()?)
}

struct ExistingUsage {
    id: i64,
    account: i64,
    agent: String,
    model: i64,
    occurred_at: i64,
    tokens: TokenUsage,
    seq: i64,
}

#[allow(clippy::too_many_arguments)]
fn apply_usage(
    tx: &Transaction<'_>,
    models: &mut ModelCache,
    s: &mut ApplySummary,
    agent: &str,
    install_key: &str,
    current_account: i64,
    session_id: Option<i64>,
    ev: &UsageEvent,
    now: UnixMs,
) -> Result<()> {
    let existing = tx
        .query_row(
            "SELECT id, usage_account_id, agent_id, model_id, occurred_at, input_tokens, output_tokens,
                    cache_read_tokens, cache_write_tokens, reasoning_tokens, source_seq
             FROM usage_records WHERE dedup_key = ?1",
            [&ev.dedup_key],
            |r| {
                Ok(ExistingUsage {
                    id: r.get(0)?,
                    account: r.get(1)?,
                    agent: r.get(2)?,
                    model: r.get(3)?,
                    occurred_at: r.get(4)?,
                    tokens: TokenUsage {
                        input: from_i64(r.get(5)?),
                        output: from_i64(r.get(6)?),
                        cache_read: from_i64(r.get(7)?),
                        cache_write: from_i64(r.get(8)?),
                        reasoning: from_i64(r.get(9)?),
                    },
                    seq: r.get(10)?,
                })
            },
        )
        .optional()?;
    let t = &ev.tokens;
    let cols = [t.input, t.output, t.cache_read, t.cache_write, t.reasoning].map(|v| v.map(to_i64));

    match existing {
        None => {
            let model_id = models.id(tx, ev.model.as_deref())?;
            let acc = account_at(tx, agent, install_key, ev.occurred_at, current_account)?;
            tx.execute(
                "INSERT INTO usage_records(usage_account_id, agent_id, session_id, model_id, occurred_at,
                     input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens,
                     source_kind, stability, dedup_key, source_seq, collected_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    acc,
                    agent,
                    session_id,
                    model_id,
                    ev.occurred_at,
                    cols[0],
                    cols[1],
                    cols[2],
                    cols[3],
                    cols[4],
                    ev.source.as_str(),
                    ev.stability.as_str(),
                    ev.dedup_key,
                    ev.source_seq,
                    now
                ],
            )?;
            add_hourly(tx, acc, agent, model_id, hour_bucket(ev.occurred_at), TokenDelta::of(t))?;
            s.usage_inserted += 1;
        }
        Some(e) if ev.source_seq <= e.seq => {
            // Duplicate or stale revision: no-op, so replays can never double-count.
            s.usage_unchanged += 1;
        }
        Some(e) => {
            // Newer revision replaces every token field. A revision without a model keeps the known one.
            let model_id = match ev.model.as_deref() {
                Some(_) => models.id(tx, ev.model.as_deref())?,
                None => e.model,
            };
            tx.execute(
                "UPDATE usage_records SET session_id = COALESCE(?1, session_id), model_id = ?2, occurred_at = ?3,
                     input_tokens = ?4, output_tokens = ?5, cache_read_tokens = ?6, cache_write_tokens = ?7,
                     reasoning_tokens = ?8, source_kind = ?9, stability = ?10, source_seq = ?11, collected_at = ?12
                 WHERE id = ?13",
                params![
                    session_id,
                    model_id,
                    ev.occurred_at,
                    cols[0],
                    cols[1],
                    cols[2],
                    cols[3],
                    cols[4],
                    ev.source.as_str(),
                    ev.stability.as_str(),
                    ev.source_seq,
                    now,
                    e.id
                ],
            )?;
            // Attribution never changes on revision: the record stays with its original account.
            let old_hour = hour_bucket(e.occurred_at);
            let new_hour = hour_bucket(ev.occurred_at);
            if e.model == model_id && old_hour == new_hour {
                add_hourly(tx, e.account, &e.agent, model_id, new_hour, TokenDelta::revision(&e.tokens, t))?;
            } else {
                add_hourly(tx, e.account, &e.agent, e.model, old_hour, TokenDelta::of(&e.tokens).negate())?;
                add_hourly(tx, e.account, &e.agent, model_id, new_hour, TokenDelta::of(t))?;
            }
            s.usage_revised += 1;
        }
    }
    Ok(())
}

fn add_hourly(
    tx: &Transaction<'_>,
    account: i64,
    agent: &str,
    model: i64,
    hour: UnixMs,
    d: TokenDelta,
) -> Result<()> {
    if d.is_zero() {
        return Ok(());
    }
    let f = d.fields;
    tx.execute(
        "INSERT INTO usage_hourly(usage_account_id, agent_id, model_id, hour_utc, input_tokens, output_tokens,
             cache_read_tokens, cache_write_tokens, reasoning_tokens, record_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(usage_account_id, agent_id, model_id, hour_utc) DO UPDATE SET
             input_tokens = input_tokens + excluded.input_tokens,
             output_tokens = output_tokens + excluded.output_tokens,
             cache_read_tokens = cache_read_tokens + excluded.cache_read_tokens,
             cache_write_tokens = cache_write_tokens + excluded.cache_write_tokens,
             reasoning_tokens = reasoning_tokens + excluded.reasoning_tokens,
             record_count = record_count + excluded.record_count",
        params![account, agent, model, hour, f[0], f[1], f[2], f[3], f[4], d.records],
    )?;
    if d.records < 0 {
        tx.execute(
            "DELETE FROM usage_hourly WHERE usage_account_id = ?1 AND agent_id = ?2 AND model_id = ?3
                 AND hour_utc = ?4 AND record_count <= 0",
            params![account, agent, model, hour],
        )?;
    }
    Ok(())
}

/// Resolves model names to ids within one batch; `None`/empty maps to the `unknown` sentinel.
struct ModelCache {
    provider: &'static str,
    ids: HashMap<String, i64>,
}

impl ModelCache {
    fn new(provider: &'static str) -> Self {
        Self { provider, ids: HashMap::new() }
    }

    fn id(&mut self, tx: &Transaction<'_>, name: Option<&str>) -> Result<i64> {
        let name = name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or("unknown");
        if let Some(id) = self.ids.get(name) {
            return Ok(*id);
        }
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM models WHERE provider_id = ?1 AND name = ?2",
                params![self.provider, name],
                |r| r.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => id,
            None => tx.query_row(
                "INSERT INTO models(provider_id, name) VALUES (?1, ?2) RETURNING id",
                params![self.provider, name],
                |r| r.get(0),
            )?,
        };
        self.ids.insert(name.to_owned(), id);
        Ok(id)
    }
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn from_i64(v: Option<i64>) -> Option<u64> {
    v.map(|x| u64::try_from(x).unwrap_or(0))
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn segment_actions() {
        assert_eq!(segment_action(None, 5), SegmentAction::Open);
        assert_eq!(segment_action(Some((10, true)), 5), SegmentAction::DropLate);
        assert_eq!(segment_action(Some((10, false)), 10), SegmentAction::DropLate);
        assert_eq!(segment_action(Some((10, true)), 10), SegmentAction::Extend);
        assert_eq!(segment_action(Some((10, true)), 20), SegmentAction::Extend);
        assert_eq!(segment_action(Some((10, false)), 20), SegmentAction::Open);
    }
}
