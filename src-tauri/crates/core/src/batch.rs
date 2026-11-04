//! `NormalizedBatch`: the only shape in which collected data reaches storage.
//!
//! A batch is complete and self-contained: it carries its own cursor advances,
//! and the store applies it in a single transaction (architecture §3.2, §5.4).
//! Every key that could identify content or a person (session ids, paths, install
//! locations) must already be hashed by the collector with
//! [`crate::hash::stable_hash`].

use crate::identity::{AccountResolution, IdentityEvidence};
use crate::ids::{AgentId, CollectorId};
use crate::metric::{SourceKind, Stability, UnavailableReason};
use crate::time::UnixMs;
use crate::usage::TokenUsage;

#[derive(Debug, Clone)]
pub struct NormalizedBatch {
    pub collector_id: CollectorId,
    pub agent: AgentId,
    /// Must be one of the agent's `allowed_scopes` in the registry.
    pub entitlement_scope: String,
    /// Hashed identifier of the installation (e.g. hash of the tool's home directory).
    pub install_key: String,
    pub identity: IdentityEvidence,
    pub detected_version: Option<String>,
    pub collected_at: UnixMs,
    pub sessions: Vec<SessionObservation>,
    pub usage: Vec<UsageEvent>,
    pub quotas: Vec<QuotaObservation>,
    pub contexts: Vec<ContextObservation>,
    pub availability: Vec<AvailabilityUpdate>,
    pub cursors: Vec<CursorUpdate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Active,
    Idle,
    Ended,
    Unknown,
}

impl SessionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Ended => "ended",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionObservation {
    /// Hashed session id.
    pub session_key: String,
    pub model: Option<String>,
    pub started_at: UnixMs,
    pub last_activity_at: UnixMs,
    pub ended_at: Option<UnixMs>,
    pub status: SessionStatus,
}

#[derive(Debug, Clone)]
pub struct UsageEvent {
    /// Globally unique per logical usage record, e.g. `claude:{message.id}`.
    pub dedup_key: String,
    /// Monotonic revision order for the same `dedup_key`; a higher value replaces the record.
    pub source_seq: i64,
    pub session_key: Option<String>,
    pub model: Option<String>,
    pub occurred_at: UnixMs,
    pub tokens: TokenUsage,
    pub source: SourceKind,
    pub stability: Stability,
}

#[derive(Debug, Clone)]
pub struct QuotaObservation {
    /// e.g. `"5h"`, `"weekly"`, `"model:<name>"`.
    pub window_kind: String,
    pub used_pct: f64,
    pub resets_at: Option<UnixMs>,
    pub observed_at: UnixMs,
    pub source: SourceKind,
    pub stability: Stability,
}

#[derive(Debug, Clone)]
pub struct ContextObservation {
    pub session_key: String,
    pub used_tokens: u64,
    pub total_tokens: Option<u64>,
    /// Source of `total_tokens` (often `Estimated` from a model table).
    pub total_source: SourceKind,
    pub observed_at: UnixMs,
    pub source: SourceKind,
    pub stability: Stability,
}

/// Current-state declaration of whether a metric is available for this account.
/// Never written as history.
#[derive(Debug, Clone)]
pub struct AvailabilityUpdate {
    /// Metric name, e.g. `"tokens"`, `"quota"`, `"context"`.
    pub metric: String,
    pub availability: Availability,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Availability {
    Available { source: SourceKind, stability: Stability },
    Unavailable(UnavailableReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorUpdate {
    /// Hashed source path.
    pub source_key: String,
    pub state: CursorState,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CursorState {
    pub byte_offset: u64,
    pub file_id: String,
    pub size: u64,
    pub mtime: UnixMs,
}

/// A batch plus the engine's account resolution, ready for the writer.
#[derive(Debug, Clone)]
pub struct ResolvedBatch {
    pub batch: NormalizedBatch,
    pub account: AccountResolution,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    #[error("empty dedup key")]
    EmptyDedupKey,
    #[error("observation uses the Unavailable source; unavailability must not be written as history")]
    UnavailableObservation,
    #[error("quota percentage out of range")]
    QuotaOutOfRange,
    #[error("empty key")]
    EmptyKey,
}

impl NormalizedBatch {
    /// Structural checks shared by engine (before submit) and store (before apply).
    pub fn validate(&self) -> Result<(), BatchError> {
        if self.install_key.is_empty() || self.entitlement_scope.is_empty() {
            return Err(BatchError::EmptyKey);
        }
        for u in &self.usage {
            if u.dedup_key.is_empty() {
                return Err(BatchError::EmptyDedupKey);
            }
            if !u.source.is_observation() {
                return Err(BatchError::UnavailableObservation);
            }
        }
        for q in &self.quotas {
            if !q.source.is_observation() {
                return Err(BatchError::UnavailableObservation);
            }
            // Providers can report slightly over 100% when a limit is exceeded; anything
            // outside this range is a parsing bug, not data.
            if !q.used_pct.is_finite() || q.used_pct < 0.0 || q.used_pct > 1000.0 {
                return Err(BatchError::QuotaOutOfRange);
            }
            if q.window_kind.is_empty() {
                return Err(BatchError::EmptyKey);
            }
        }
        for c in &self.contexts {
            if !c.source.is_observation() {
                return Err(BatchError::UnavailableObservation);
            }
            if c.session_key.is_empty() {
                return Err(BatchError::EmptyKey);
            }
        }
        if self.sessions.iter().any(|s| s.session_key.is_empty())
            || self.cursors.iter().any(|c| c.source_key.is_empty())
        {
            return Err(BatchError::EmptyKey);
        }
        Ok(())
    }
}
