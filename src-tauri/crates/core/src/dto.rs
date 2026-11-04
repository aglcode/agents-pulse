//! Data transfer objects sent to the frontend. TypeScript types are generated
//! from these by ts-rs into `src/ipc/bindings/` when `cargo test` runs.

use serde::Serialize;
use ts_rs::TS;

use crate::identity::AccountKind;
use crate::ids::{AgentId, ProviderId};
use crate::metric::{CollectionError, Metric, UnavailableReason};
use crate::time::UnixMs;

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct OverviewDto {
    pub generated_at: UnixMs,
    pub accounts: Vec<AccountSummaryDto>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct AccountSummaryDto {
    pub usage_account_id: i64,
    pub provider: ProviderId,
    pub entitlement_scope: String,
    pub kind: AccountKind,
    pub display_label: String,
    /// Label of the signed-in identity, if verified (used for grouping only).
    pub identity_label: Option<String>,
    pub identity_id: Option<i64>,
    /// Active link group, if the user linked this account with others.
    pub link_group_id: Option<i64>,
    pub agents: Vec<AgentId>,
    pub tokens_today: Metric<TokenTotalsDto>,
    pub quotas: Vec<QuotaDto>,
    /// Set when the collector declared quota unavailable for this account.
    pub quota_unavailable_reason: Option<UnavailableReason>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct TokenTotalsDto {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub reasoning: i64,
    pub records: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct QuotaDto {
    pub window_kind: String,
    pub used_pct: Metric<f64>,
    pub resets_at: Option<UnixMs>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct CollectorStatusDto {
    pub collector_id: String,
    pub state: String,
    pub detected_version: Option<String>,
    pub last_success_at: Option<UnixMs>,
    pub last_error: Option<CollectionError>,
    pub consecutive_failures: i64,
    pub next_run_at: Option<UnixMs>,
}

/// Dashboard: one card per AI tool (agent).
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct DashboardDto {
    pub generated_at: UnixMs,
    pub agents: Vec<AgentCardDto>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct AgentCardDto {
    pub agent: AgentId,
    pub display_name: String,
    pub provider: ProviderId,
    /// `None` when this build has no collector for the agent yet.
    pub collector: Option<CollectorCardDto>,
    /// Shown when `collector` is `None`, e.g. "Collector planned for M5."
    pub planned_note: Option<String>,
    /// Usage accounts currently bound to this agent.
    pub accounts: Vec<AccountSummaryDto>,
    /// Per-model breakdown of today's tokens (only while the tokens metric is available).
    pub models_today: Vec<ModelUsageDto>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct CollectorCardDto {
    pub collector_id: String,
    pub enabled: bool,
    pub status: Option<CollectorStatusDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct ModelUsageDto {
    pub model: String,
    pub input: i64,
    pub output: i64,
    pub records: i64,
}

/// Payload of the `engine://data-changed` event: which data changed, so the UI
/// can refetch just those slices.
#[derive(Debug, Clone, Default, Serialize, TS)]
#[ts(export)]
pub struct DataChangedPayload {
    pub collector_ids: Vec<String>,
    pub usage_account_ids: Vec<i64>,
}
