//! The shared collector interface (architecture §6).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::batch::{CursorState, NormalizedBatch};
use crate::identity::IdentityEvidence;
use crate::ids::{AgentId, CollectorId};
use crate::metric::{ErrorCode, SourceKind, Stability};

#[derive(Debug, Clone)]
pub enum Trigger {
    /// Poll at this base interval (stretched while the window is hidden).
    Interval(Duration),
    /// Run when files under these paths change (implemented from M3).
    WatchPaths(Vec<PathBuf>),
    /// Run on explicit refresh requests.
    Manual,
}

/// Declares which metrics a collector can provide and from which source.
/// `None` means the provider doesn't expose that metric.
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    pub metrics: Vec<(&'static str, Option<(SourceKind, Stability)>)>,
}

#[derive(Debug, Clone)]
pub struct InstallDetection {
    /// Hashed install location.
    pub install_key: String,
    pub version: Option<String>,
    pub running: bool,
    pub identity: IdentityEvidence,
}

#[derive(Debug, Clone)]
pub enum Detection {
    NotInstalled,
    Installed(Vec<InstallDetection>),
}

/// Everything a collector run may read besides the provider itself.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    pub home_dir: Option<PathBuf>,
    /// Last committed cursor per hashed source key, for incremental reads.
    pub cursors: HashMap<String, CursorState>,
    /// Result of `detect()` for this run.
    pub installs: Vec<InstallDetection>,
}

/// Collector failures. Variants carry codes and small structured facts only:
/// never raw input, file contents, paths or secrets (architecture §6.2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CollectorError {
    #[error("provider not installed")]
    NotInstalled,
    #[error("provider not running")]
    NotRunning,
    #[error("unsupported provider version {detected}")]
    UnsupportedVersion { detected: String },
    #[error("could not parse {file_kind} (line {line:?})")]
    Parse { file_kind: &'static str, line: Option<u64> },
    #[error("i/o error: {0:?}")]
    Io(std::io::ErrorKind),
    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },
    #[error("cancelled")]
    Cancelled,
    #[error("internal error: {0}")]
    Internal(&'static str),
}

impl CollectorError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::NotInstalled => ErrorCode::NotInstalled,
            Self::NotRunning => ErrorCode::NotRunning,
            Self::UnsupportedVersion { .. } => ErrorCode::UnsupportedVersion,
            Self::Parse { .. } => ErrorCode::Parse,
            Self::Io(_) => ErrorCode::Io,
            Self::RateLimited { .. } => ErrorCode::RateLimited,
            Self::Cancelled | Self::Internal(_) => ErrorCode::Internal,
        }
    }
}

#[async_trait]
pub trait Collector: Send + Sync {
    fn id(&self) -> CollectorId;
    fn agent(&self) -> AgentId;
    fn capabilities(&self) -> Capabilities;
    fn triggers(&self) -> Vec<Trigger>;

    /// Upper bound for one `detect()` + `collect()` run.
    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    /// Cheap check: installed? running? which version and identity per installation?
    async fn detect(&self, ctx: &Ctx) -> Detection;

    /// Read available data and return one complete batch per installation.
    /// Implementations should check `cancel` at await points.
    async fn collect(
        &self,
        ctx: &Ctx,
        cancel: &CancellationToken,
    ) -> Result<Vec<NormalizedBatch>, CollectorError>;
}
