//! Read queries that build frontend DTOs. Every metric is built through `Metric`
//! constructors so its source, freshness and unavailability reason travel with it.

use std::collections::HashMap;

use agentpulse_core::batch::CursorState;
use agentpulse_core::dto::{
    AccountSummaryDto, CollectorStatusDto, ModelUsageDto, OverviewDto, QuotaDto, TokenTotalsDto,
};
use agentpulse_core::identity::AccountKind;
use agentpulse_core::ids::{AgentId, ProviderId};
use agentpulse_core::metric::{
    CollectionError, ErrorCode, Metric, SourceKind, Stability, UnavailableReason, Unit,
};
use agentpulse_core::time::{local_day_start_utc, UnixMs};
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::Result;

/// How old a value may be before it is presented as stale.
#[derive(Debug, Clone, Copy)]
pub struct FreshnessPolicy {
    pub quota_max_age_ms: i64,
    pub usage_max_age_ms: i64,
}

impl Default for FreshnessPolicy {
    fn default() -> Self {
        Self { quota_max_age_ms: 10 * 60_000, usage_max_age_ms: 10 * 60_000 }
    }
}

struct AvailabilityRow {
    collector_id: String,
    state: String,
    source: Option<String>,
    stability: Option<String>,
    reason: Option<String>,
}

/// (last_success_at, last_error_code, last_error_message, last_error_at)
type StatusRow = (Option<i64>, Option<String>, Option<String>, Option<i64>);

struct CollectorHealth {
    last_success_at: Option<UnixMs>,
    current_error: Option<CollectionError>,
}

pub fn overview(conn: &Connection, now: UnixMs, utc_offset_minutes: i32, policy: FreshnessPolicy) -> Result<OverviewDto> {
    let day_start = local_day_start_utc(now, utc_offset_minutes);
    let mut stmt = conn.prepare(
        "SELECT ua.id, ua.provider_id, ua.entitlement_scope, ua.kind, ua.display_label, ua.identity_id,
                i.display_label, ua.last_collected_at,
                (SELECT m.group_id FROM link_memberships m
                 WHERE m.usage_account_id = ua.id AND m.unlinked_at IS NULL)
         FROM usage_accounts ua LEFT JOIN identities i ON i.id = ua.identity_id
         WHERE ua.status = 'active'
         ORDER BY ua.provider_id, ua.id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<i64>>(8)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut accounts = Vec::with_capacity(rows.len());
    for (id, provider, scope, kind, label, identity_id, identity_label, last_collected, group) in rows {
        let Some(provider) = parse_provider(&provider) else { continue };
        let agents = agents_for(conn, id)?;
        let tokens_today = tokens_metric(conn, id, day_start, last_collected, now, policy)?;
        let quotas = quotas_for(conn, id, now, policy)?;
        let quota_unavailable_reason = availability(conn, id, "quota")?
            .filter(|a| a.state == "unavailable")
            .map(|a| parse_reason(a.reason.as_deref()));
        accounts.push(AccountSummaryDto {
            usage_account_id: id,
            provider,
            entitlement_scope: scope,
            kind: if kind == "verified" { AccountKind::Verified } else { AccountKind::InstallScoped },
            display_label: label,
            identity_label,
            identity_id,
            link_group_id: group,
            agents,
            tokens_today,
            quotas,
            quota_unavailable_reason,
        });
    }
    Ok(OverviewDto { generated_at: now, accounts })
}

fn agents_for(conn: &Connection, account: i64) -> Result<Vec<AgentId>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT agent_id FROM agent_bindings
         WHERE usage_account_id = ?1 AND unbound_at IS NULL ORDER BY agent_id",
    )?;
    let ids = stmt
        .query_map([account], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(ids.iter().filter_map(|s| parse_agent(s)).collect())
}

fn tokens_metric(
    conn: &Connection,
    account: i64,
    since: UnixMs,
    last_collected: Option<UnixMs>,
    now: UnixMs,
    policy: FreshnessPolicy,
) -> Result<Metric<TokenTotalsDto>> {
    let Some(avail) = availability(conn, account, "tokens")? else {
        return Ok(Metric::unavailable(Unit::Tokens, UnavailableReason::NotYetCollected));
    };
    if avail.state != "available" {
        return Ok(Metric::unavailable(Unit::Tokens, parse_reason(avail.reason.as_deref())));
    }
    // "Today" is computed from raw records so non-whole-hour time zones are exact.
    let totals = conn.query_row(
        "SELECT IFNULL(SUM(input_tokens), 0), IFNULL(SUM(output_tokens), 0), IFNULL(SUM(cache_read_tokens), 0),
                IFNULL(SUM(cache_write_tokens), 0), IFNULL(SUM(reasoning_tokens), 0), COUNT(*)
         FROM usage_records WHERE usage_account_id = ?1 AND occurred_at >= ?2",
        params![account, since],
        |r| {
            Ok(TokenTotalsDto {
                input: r.get(0)?,
                output: r.get(1)?,
                cache_read: r.get(2)?,
                cache_write: r.get(3)?,
                reasoning: r.get(4)?,
                records: r.get(5)?,
            })
        },
    )?;
    let health = collector_health(conn, &avail.collector_id)?;
    let observed_at = last_collected.unwrap_or(now);
    let metric = build_metric(totals, Unit::Tokens, &avail, observed_at)?;
    Ok(metric
        .with_last_success(health.last_success_at)
        .with_last_error(health.current_error)
        .evaluate_freshness(now, policy.usage_max_age_ms))
}

fn quotas_for(conn: &Connection, account: i64, now: UnixMs, policy: FreshnessPolicy) -> Result<Vec<QuotaDto>> {
    let mut stmt = conn.prepare_cached(
        "SELECT q.window_kind, q.used_pct, q.resets_at, q.source_kind, q.stability, q.last_observed_at
         FROM quota_segments q
         WHERE q.usage_account_id = ?1
           AND q.last_observed_at = (SELECT MAX(q2.last_observed_at) FROM quota_segments q2
                                     WHERE q2.usage_account_id = q.usage_account_id AND q2.window_kind = q.window_kind)
         ORDER BY q.window_kind",
    )?;
    let rows = stmt
        .query_map([account], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let avail = availability(conn, account, "quota")?;
    // A disabled or failing-as-unavailable collector: keep history, but never present it as current.
    if avail.as_ref().is_some_and(|a| a.state != "available") {
        return Ok(Vec::new());
    }
    let health = match &avail {
        Some(a) => Some(collector_health(conn, &a.collector_id)?),
        None => None,
    };
    let mut out = Vec::with_capacity(rows.len());
    for (window_kind, pct, resets_at, source, stability, last_observed) in rows {
        let row = AvailabilityRow {
            collector_id: String::new(),
            state: "available".into(),
            source: Some(source),
            stability: Some(stability),
            reason: None,
        };
        let mut m = build_metric(pct, Unit::Percent, &row, last_observed)?;
        if let Some(h) = &health {
            m = m.with_last_success(h.last_success_at).with_last_error(h.current_error.clone());
        }
        out.push(QuotaDto {
            window_kind,
            used_pct: m.evaluate_freshness(now, policy.quota_max_age_ms),
            resets_at,
        });
    }
    Ok(out)
}

fn build_metric<T>(value: T, unit: Unit, avail: &AvailabilityRow, observed_at: UnixMs) -> Result<Metric<T>> {
    let source = avail.source.as_deref().and_then(SourceKind::parse).unwrap_or(SourceKind::Local);
    let stability = avail.stability.as_deref().and_then(Stability::parse).unwrap_or(Stability::Undocumented);
    let m = match source {
        SourceKind::Estimated => {
            Metric::estimated(value, unit, "estimated by collector", stability, observed_at, observed_at)
        }
        SourceKind::Unavailable => return Ok(Metric::unavailable(unit, UnavailableReason::CollectionFailed)),
        s => Metric::observed(value, unit, s, stability, observed_at, observed_at),
    };
    // Constructors only fail on programmer error (handled above); fall back to "unavailable".
    Ok(m.unwrap_or_else(|_| Metric::unavailable(unit, UnavailableReason::CollectionFailed)))
}

fn availability(conn: &Connection, account: i64, metric: &str) -> Result<Option<AvailabilityRow>> {
    // Prefer an available declaration if several collectors report on the same account.
    Ok(conn
        .prepare_cached(
            "SELECT collector_id, state, source_kind, stability, reason FROM metric_availability
             WHERE usage_account_id = ?1 AND metric = ?2
             ORDER BY (state = 'available') DESC, since DESC LIMIT 1",
        )?
        .query_row(params![account, metric], |r| {
            Ok(AvailabilityRow {
                collector_id: r.get(0)?,
                state: r.get(1)?,
                source: r.get(2)?,
                stability: r.get(3)?,
                reason: r.get(4)?,
            })
        })
        .optional()?)
}

fn collector_health(conn: &Connection, collector_id: &str) -> Result<CollectorHealth> {
    let row: Option<StatusRow> = conn
        .prepare_cached(
            "SELECT last_success_at, last_error_code, last_error_message, last_error_at
             FROM collector_status WHERE collector_id = ?1",
        )?
        .query_row([collector_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?;
    let Some((last_success_at, code, message, at)) = row else {
        return Ok(CollectorHealth { last_success_at: None, current_error: None });
    };
    // An error only counts as "current" if it happened after the last success.
    let current_error = match (code, at) {
        (Some(code), Some(at)) if last_success_at.is_none_or(|s| at > s) => Some(CollectionError {
            code: ErrorCode::parse(&code).unwrap_or(ErrorCode::Internal),
            at,
            message: message.unwrap_or_default(),
        }),
        _ => None,
    };
    Ok(CollectorHealth { last_success_at, current_error })
}

pub fn collector_statuses(conn: &Connection) -> Result<Vec<CollectorStatusDto>> {
    let mut stmt = conn.prepare(
        "SELECT collector_id, state, detected_version, last_success_at, last_error_code, last_error_message,
                last_error_at, consecutive_failures, next_run_at
         FROM collector_status ORDER BY collector_id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let code: Option<String> = r.get(4)?;
            let message: Option<String> = r.get(5)?;
            let at: Option<i64> = r.get(6)?;
            Ok(CollectorStatusDto {
                collector_id: r.get(0)?,
                state: r.get(1)?,
                detected_version: r.get(2)?,
                last_success_at: r.get(3)?,
                last_error: match (code, at) {
                    (Some(c), Some(at)) => Some(CollectionError {
                        code: ErrorCode::parse(&c).unwrap_or(ErrorCode::Internal),
                        at,
                        message: message.unwrap_or_default(),
                    }),
                    _ => None,
                },
                consecutive_failures: r.get(7)?,
                next_run_at: r.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Per-agent, per-model token totals since the start of the local day.
/// Only agents whose `tokens` metric is currently available are included, so a
/// disabled collector's old numbers are never shown as today's usage.
pub fn models_today(conn: &Connection, now: UnixMs, utc_offset_minutes: i32) -> Result<Vec<(AgentId, ModelUsageDto)>> {
    let since = local_day_start_utc(now, utc_offset_minutes);
    let mut stmt = conn.prepare_cached(
        "SELECT r.agent_id, m.name, IFNULL(SUM(r.input_tokens), 0), IFNULL(SUM(r.output_tokens), 0), COUNT(*)
         FROM usage_records r JOIN models m ON m.id = r.model_id
         WHERE r.occurred_at >= ?1
           AND EXISTS (SELECT 1 FROM metric_availability a
                       WHERE a.usage_account_id = r.usage_account_id AND a.metric = 'tokens' AND a.state = 'available')
         GROUP BY r.agent_id, m.name
         ORDER BY r.agent_id, SUM(IFNULL(r.input_tokens, 0) + IFNULL(r.output_tokens, 0)) DESC",
    )?;
    let rows = stmt
        .query_map([since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                ModelUsageDto { model: r.get(1)?, input: r.get(2)?, output: r.get(3)?, records: r.get(4)? },
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().filter_map(|(a, m)| parse_agent(&a).map(|a| (a, m))).collect())
}

/// Raw JSON value of an application setting.
pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached("SELECT value FROM app_settings WHERE key = ?1")?
        .query_row([key], |r| r.get(0))
        .optional()?)
}

pub fn cursors_for(conn: &Connection, collector_id: &str) -> Result<HashMap<String, CursorState>> {
    let mut stmt = conn.prepare_cached(
        "SELECT source_key, byte_offset, file_id, size, mtime FROM collector_cursors WHERE collector_id = ?1",
    )?;
    let rows = stmt
        .query_map([collector_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                CursorState {
                    byte_offset: u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                    file_id: r.get(2)?,
                    size: u64::try_from(r.get::<_, i64>(3)?).unwrap_or(0),
                    mtime: r.get(4)?,
                },
            ))
        })?
        .collect::<std::result::Result<HashMap<_, _>, _>>()?;
    Ok(rows)
}

fn parse_reason(json: Option<&str>) -> UnavailableReason {
    json.and_then(|j| serde_json::from_str(j).ok()).unwrap_or(UnavailableReason::CollectionFailed)
}

fn parse_provider(s: &str) -> Option<ProviderId> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
}

fn parse_agent(s: &str) -> Option<AgentId> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
}
