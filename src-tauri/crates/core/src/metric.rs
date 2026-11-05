//! `Metric<T>`: a value together with everything needed to present it truthfully
//! (architecture §3.4). Constructors enforce the invariants, so an unavailable
//! metric can never carry a value and an estimate always explains its basis.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::time::UnixMs;

/// Blueprint §7 source classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SourceKind {
    OfficialApi,
    OfficialTelemetry,
    Local,
    Estimated,
    Unavailable,
}

impl SourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OfficialApi => "official_api",
            Self::OfficialTelemetry => "official_telemetry",
            Self::Local => "local",
            Self::Estimated => "estimated",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "official_api" => Self::OfficialApi,
            "official_telemetry" => Self::OfficialTelemetry,
            "local" => Self::Local,
            "estimated" => Self::Estimated,
            "unavailable" => Self::Unavailable,
            _ => return None,
        })
    }

    /// Sources that represent an actual observation and may be written to history.
    pub const fn is_observation(self) -> bool {
        !matches!(self, Self::Unavailable)
    }
}

/// Whether the interface a value came from is documented by the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Stability {
    Documented,
    Undocumented,
}

impl Stability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Documented => "documented",
            Self::Undocumented => "undocumented",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "documented" => Some(Self::Documented),
            "undocumented" => Some(Self::Undocumented),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Unit {
    Tokens,
    Percent,
    Timestamp,
    Count,
    DurationSecs,
}

/// Computed at read time from `observed_at` and a per-metric max age. Never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum Freshness {
    Fresh,
    Stale { since: UnixMs },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum UnavailableReason {
    NotExposedByProvider,
    ProviderNotInstalled,
    ProviderNotRunning,
    NotAuthenticated,
    UnsupportedVersion { detected: String },
    OptInRequired,
    CollectorDisabled,
    CollectionFailed,
    NotYetCollected,
    /// A quota window's reset time has passed since the last reading, so its
    /// current value is unknown (and is likely lower than the last reading).
    ResetSinceLastReading,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ErrorCode {
    NotInstalled,
    NotRunning,
    UnsupportedVersion,
    Parse,
    Io,
    Timeout,
    Panicked,
    RateLimited,
    Storage,
    InvalidBatch,
    Internal,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotInstalled => "not_installed",
            Self::NotRunning => "not_running",
            Self::UnsupportedVersion => "unsupported_version",
            Self::Parse => "parse",
            Self::Io => "io",
            Self::Timeout => "timeout",
            Self::Panicked => "panicked",
            Self::RateLimited => "rate_limited",
            Self::Storage => "storage",
            Self::InvalidBatch => "invalid_batch",
            Self::Internal => "internal",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "not_installed" => Self::NotInstalled,
            "not_running" => Self::NotRunning,
            "unsupported_version" => Self::UnsupportedVersion,
            "parse" => Self::Parse,
            "io" => Self::Io,
            "timeout" => Self::Timeout,
            "panicked" => Self::Panicked,
            "rate_limited" => Self::RateLimited,
            "storage" => Self::Storage,
            "invalid_batch" => Self::InvalidBatch,
            "internal" => Self::Internal,
            _ => return None,
        })
    }
}

/// Most recent collection failure. `message` is sanitized: no paths, content or secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CollectionError {
    pub code: ErrorCode,
    pub at: UnixMs,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetricError {
    #[error("observed metrics must use an observation source, not {0:?}")]
    InvalidSource(SourceKind),
    #[error("estimated metrics require a non-empty estimation basis")]
    MissingEstimationBasis,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct Metric<T> {
    value: Option<T>,
    unit: Unit,
    source: SourceKind,
    stability: Option<Stability>,
    observed_at: Option<UnixMs>,
    collected_at: Option<UnixMs>,
    last_success_at: Option<UnixMs>,
    freshness: Freshness,
    unavailable_reason: Option<UnavailableReason>,
    estimation_basis: Option<String>,
    last_error: Option<CollectionError>,
}

impl<T> Metric<T> {
    /// A value read from a provider source. `source` must not be `Estimated` or `Unavailable`.
    pub fn observed(
        value: T,
        unit: Unit,
        source: SourceKind,
        stability: Stability,
        observed_at: UnixMs,
        collected_at: UnixMs,
    ) -> Result<Self, MetricError> {
        if matches!(source, SourceKind::Estimated | SourceKind::Unavailable) {
            return Err(MetricError::InvalidSource(source));
        }
        Ok(Self {
            value: Some(value),
            unit,
            source,
            stability: Some(stability),
            observed_at: Some(observed_at),
            collected_at: Some(collected_at),
            last_success_at: Some(collected_at),
            freshness: Freshness::Unknown,
            unavailable_reason: None,
            estimation_basis: None,
            last_error: None,
        })
    }

    /// A value AgentPulse calculated. `basis` explains how and is shown to the user.
    pub fn estimated(
        value: T,
        unit: Unit,
        basis: impl Into<String>,
        stability: Stability,
        observed_at: UnixMs,
        collected_at: UnixMs,
    ) -> Result<Self, MetricError> {
        let basis = basis.into();
        if basis.trim().is_empty() {
            return Err(MetricError::MissingEstimationBasis);
        }
        Ok(Self {
            value: Some(value),
            unit,
            source: SourceKind::Estimated,
            stability: Some(stability),
            observed_at: Some(observed_at),
            collected_at: Some(collected_at),
            last_success_at: Some(collected_at),
            freshness: Freshness::Unknown,
            unavailable_reason: None,
            estimation_basis: Some(basis),
            last_error: None,
        })
    }

    /// No value, with the reason shown to the user.
    pub fn unavailable(unit: Unit, reason: UnavailableReason) -> Self {
        Self {
            value: None,
            unit,
            source: SourceKind::Unavailable,
            stability: None,
            observed_at: None,
            collected_at: None,
            last_success_at: None,
            freshness: Freshness::Unknown,
            unavailable_reason: Some(reason),
            estimation_basis: None,
            last_error: None,
        }
    }

    pub fn with_last_success(mut self, at: Option<UnixMs>) -> Self {
        self.last_success_at = at;
        self
    }

    /// Attach the most recent failure. The last good value (if any) is kept, and
    /// freshness evaluation will mark it stale once it ages out.
    pub fn with_last_error(mut self, err: Option<CollectionError>) -> Self {
        self.last_error = err;
        self
    }

    /// Compute freshness: stale once `now - observed_at > max_age_ms`.
    pub fn evaluate_freshness(mut self, now: UnixMs, max_age_ms: i64) -> Self {
        self.freshness = match self.observed_at {
            Some(at) if now - at > max_age_ms => Freshness::Stale { since: at + max_age_ms },
            Some(_) => Freshness::Fresh,
            None => Freshness::Unknown,
        };
        self
    }

    pub fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }
    pub fn unit(&self) -> Unit {
        self.unit
    }
    pub fn source(&self) -> SourceKind {
        self.source
    }
    pub fn stability(&self) -> Option<Stability> {
        self.stability
    }
    pub fn observed_at(&self) -> Option<UnixMs> {
        self.observed_at
    }
    pub fn collected_at(&self) -> Option<UnixMs> {
        self.collected_at
    }
    pub fn last_success_at(&self) -> Option<UnixMs> {
        self.last_success_at
    }
    pub fn freshness(&self) -> Freshness {
        self.freshness
    }
    pub fn unavailable_reason(&self) -> Option<&UnavailableReason> {
        self.unavailable_reason.as_ref()
    }
    pub fn estimation_basis(&self) -> Option<&str> {
        self.estimation_basis.as_deref()
    }
    pub fn last_error(&self) -> Option<&CollectionError> {
        self.last_error.as_ref()
    }

    /// True when the invariants hold. Always true for values built via constructors.
    pub fn is_consistent(&self) -> bool {
        let unavailable = self.source == SourceKind::Unavailable;
        let value_ok = self.value.is_none() == unavailable;
        let reason_ok = self.unavailable_reason.is_some() == unavailable;
        let basis_ok = (self.source == SourceKind::Estimated) == self.estimation_basis.is_some();
        value_ok && reason_ok && basis_ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_rejects_estimated_and_unavailable_sources() {
        for s in [SourceKind::Estimated, SourceKind::Unavailable] {
            let r = Metric::observed(1.0, Unit::Percent, s, Stability::Documented, 1, 1);
            assert_eq!(r.unwrap_err(), MetricError::InvalidSource(s));
        }
    }

    #[test]
    fn estimated_requires_basis() {
        let r = Metric::estimated(1.0, Unit::Percent, "  ", Stability::Documented, 1, 1);
        assert_eq!(r.unwrap_err(), MetricError::MissingEstimationBasis);
        let m = Metric::estimated(1.0, Unit::Percent, "model table", Stability::Documented, 1, 1)
            .unwrap();
        assert_eq!(m.source(), SourceKind::Estimated);
        assert!(m.is_consistent());
    }

    #[test]
    fn unavailable_has_no_value_and_a_reason() {
        let m: Metric<f64> = Metric::unavailable(Unit::Percent, UnavailableReason::NotExposedByProvider);
        assert!(m.value().is_none());
        assert_eq!(m.source(), SourceKind::Unavailable);
        assert!(m.unavailable_reason().is_some());
        assert!(m.is_consistent());
    }

    #[test]
    fn freshness_goes_stale_after_max_age() {
        let m = Metric::observed(5u64, Unit::Tokens, SourceKind::Local, Stability::Documented, 1_000, 1_000)
            .unwrap();
        assert_eq!(m.clone().evaluate_freshness(1_500, 1_000).freshness(), Freshness::Fresh);
        assert_eq!(
            m.evaluate_freshness(2_500, 1_000).freshness(),
            Freshness::Stale { since: 2_000 }
        );
    }

    #[test]
    fn failure_keeps_last_good_value() {
        let err = CollectionError { code: ErrorCode::Timeout, at: 9_000, message: "timed out".into() };
        let m = Metric::observed(42.0, Unit::Percent, SourceKind::Local, Stability::Undocumented, 1_000, 1_000)
            .unwrap()
            .with_last_error(Some(err.clone()))
            .evaluate_freshness(9_000, 1_000);
        assert_eq!(m.value(), Some(&42.0));
        assert_eq!(m.last_error(), Some(&err));
        assert!(matches!(m.freshness(), Freshness::Stale { .. }));
        assert!(m.is_consistent());
    }

    #[test]
    fn serialized_shape_exposes_source_and_reason() {
        let m: Metric<f64> = Metric::unavailable(
            Unit::Percent,
            UnavailableReason::UnsupportedVersion { detected: "1.2".into() },
        );
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["source"], "unavailable");
        assert_eq!(v["unavailable_reason"]["kind"], "unsupported_version");
        assert_eq!(v["value"], serde_json::Value::Null);
    }
}
