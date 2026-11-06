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

/// Claude Code config folders AgentPulse reads, for the folder settings on the
/// Claude Code card. Paths are shown locally only and never stored in collected data.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ClaudeFoldersDto {
    pub folders: Vec<ClaudeFolderDto>,
    /// `.claude-*` folders in the home that have session logs and aren't read yet.
    /// Offered to the user; never added automatically.
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ClaudeFolderDto {
    pub path: String,
    pub origin: ClaudeFolderOrigin,
    /// Whether the folder has a `projects` log folder (otherwise nothing is read from it).
    pub has_logs: bool,
    /// Whether AgentPulse's quota tap is this folder's status line.
    pub quota_tap: QuotaTapState,
    /// Whether this folder is read. Only Pro, Max and Ultra sign-ins are supported.
    pub read: bool,
    /// Why it isn't read, e.g. "Team plan". `None` when read.
    pub skipped_reason: Option<String>,
}

/// What "Remove from AgentPulse" will do for a folder that isn't read, shown before confirming.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ForgetFolderPreviewDto {
    pub folder: String,
    /// Accounts whose stored data will be deleted from AgentPulse's database.
    pub accounts: Vec<String>,
    /// Lines removed from the folder's `settings.json` (the quota tap), if it's on.
    pub tap_block: Option<String>,
    pub settings_path: String,
    /// `settings.json` content the preview was made from; sent back on confirm.
    pub before: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum QuotaTapState {
    Off,
    On,
    /// The user has their own status line; AgentPulse won't replace it.
    OtherStatusLine,
    /// `settings.json` can't be read as JSON.
    Unreadable,
    /// This AgentPulse build can't be run as a status line (e.g. installed in a path with spaces).
    Unsupported,
    /// Claude Code doesn't report quota for this plan (only Pro, Max and Ultra), so Enable isn't offered.
    PlanWithoutQuota,
}

/// A previewed change to a folder's `settings.json`, shown before the user confirms.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct QuotaTapPreviewDto {
    pub folder: String,
    pub settings_path: String,
    pub enable: bool,
    /// The exact lines added (enable) or removed (disable).
    pub block: String,
    /// The file content the preview was made from (`None`: file doesn't exist yet).
    /// Sent back on confirm, so nothing is applied if the file changed meanwhile.
    pub before: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ClaudeFolderOrigin {
    /// `~/.claude`, Claude Code's default.
    Default,
    /// `CLAUDE_CONFIG_DIR`, as seen by AgentPulse.
    Environment,
    /// Added by the user.
    Added,
}
