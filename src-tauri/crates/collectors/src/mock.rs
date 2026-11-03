//! Synthetic collectors for development and tests. They report under the
//! `mock` provider/agent ("Mock (dev only)"), so their data can never be mistaken
//! for a real provider. The app registers them only in debug builds.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use agentpulse_core::batch::{
    Availability, AvailabilityUpdate, ContextObservation, CursorState, CursorUpdate, NormalizedBatch,
    QuotaObservation, SessionObservation, SessionStatus, UsageEvent,
};
use agentpulse_core::collector::{
    Capabilities, Collector, CollectorError, Ctx, Detection, InstallDetection, Trigger,
};
use agentpulse_core::hash::stable_hash;
use agentpulse_core::identity::IdentityEvidence;
use agentpulse_core::ids::{AgentId, CollectorId};
use agentpulse_core::metric::{SourceKind, Stability};
use agentpulse_core::time::now_ms;
use agentpulse_core::usage::TokenUsage;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy)]
pub enum MockBehavior {
    /// Produces one usage record, a quota reading and a context reading per run.
    Healthy,
    /// Panics inside `collect()`.
    Panicking,
    /// Never finishes and ignores the cancellation token (abortable only).
    Hanging,
    /// Sleeps without checking cancellation, then returns a healthy batch.
    Slow(Duration),
}

pub struct MockCollector {
    id: String,
    behavior: MockBehavior,
    interval: Duration,
    timeout: Duration,
    seq: AtomicU64,
}

impl MockCollector {
    pub fn new(id: impl Into<String>, behavior: MockBehavior, interval: Duration) -> Self {
        Self { id: id.into(), behavior, interval, timeout: Duration::from_secs(30), seq: AtomicU64::new(0) }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn install_key(&self) -> String {
        stable_hash(&["mock-install", &self.id])
    }

    fn healthy_batch(&self) -> NormalizedBatch {
        let n = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let now = now_ms();
        let session_key = stable_hash(&["mock-session", &self.id]);
        let local = SourceKind::Local;
        let stab = Stability::Documented;
        NormalizedBatch {
            collector_id: CollectorId::new(self.id.clone()),
            agent: AgentId::Mock,
            entitlement_scope: "mock:dev".into(),
            install_key: self.install_key(),
            identity: IdentityEvidence::none("synthetic mock collector"),
            detected_version: Some("mock-1".into()),
            collected_at: now,
            sessions: vec![SessionObservation {
                session_key: session_key.clone(),
                model: Some("mock-model".into()),
                started_at: now - i64::try_from(n).unwrap_or(0) * 1_000,
                last_activity_at: now,
                ended_at: None,
                status: SessionStatus::Active,
            }],
            usage: vec![UsageEvent {
                dedup_key: format!("mock:{}:{n}", self.id),
                source_seq: 1,
                session_key: Some(session_key.clone()),
                model: Some("mock-model".into()),
                occurred_at: now,
                tokens: TokenUsage {
                    input: Some(1_000 + n * 37 % 500),
                    output: Some(200 + n * 13 % 100),
                    cache_read: Some(n * 101 % 800),
                    cache_write: None,
                    reasoning: None,
                },
                source: local,
                stability: stab,
            }],
            quotas: vec![QuotaObservation {
                window_kind: "5h".into(),
                used_pct: ((n * 3) % 100) as f64,
                resets_at: Some(now + 5 * 3_600_000),
                observed_at: now,
                source: local,
                stability: stab,
            }],
            contexts: vec![ContextObservation {
                session_key,
                used_tokens: (n * 4_000) % 200_000,
                total_tokens: Some(200_000),
                total_source: SourceKind::Estimated,
                observed_at: now,
                source: local,
                stability: stab,
            }],
            availability: ["tokens", "quota", "context"]
                .into_iter()
                .map(|m| AvailabilityUpdate {
                    metric: m.into(),
                    availability: Availability::Available { source: local, stability: stab },
                })
                .collect(),
            cursors: vec![CursorUpdate {
                source_key: stable_hash(&["mock-cursor", &self.id]),
                state: CursorState { byte_offset: n, file_id: "mock".into(), size: n, mtime: now },
            }],
        }
    }
}

#[async_trait]
impl Collector for MockCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new(self.id.clone())
    }

    fn agent(&self) -> AgentId {
        AgentId::Mock
    }

    fn capabilities(&self) -> Capabilities {
        let src = Some((SourceKind::Local, Stability::Documented));
        Capabilities { metrics: vec![("tokens", src), ("quota", src), ("context", src)] }
    }

    fn triggers(&self) -> Vec<Trigger> {
        vec![Trigger::Interval(self.interval), Trigger::Manual]
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    async fn detect(&self, _ctx: &Ctx) -> Detection {
        Detection::Installed(vec![InstallDetection {
            install_key: self.install_key(),
            version: Some("mock-1".into()),
            running: true,
            identity: IdentityEvidence::none("synthetic mock collector"),
        }])
    }

    async fn collect(&self, _ctx: &Ctx, cancel: &CancellationToken) -> Result<Vec<NormalizedBatch>, CollectorError> {
        match self.behavior {
            MockBehavior::Healthy => {
                if cancel.is_cancelled() {
                    return Err(CollectorError::Cancelled);
                }
                Ok(vec![self.healthy_batch()])
            }
            MockBehavior::Panicking => panic!("mock collector panic (intentional, for isolation tests)"),
            MockBehavior::Hanging => {
                std::future::pending::<()>().await;
                Ok(vec![])
            }
            MockBehavior::Slow(d) => {
                tokio::time::sleep(d).await;
                Ok(vec![self.healthy_batch()])
            }
        }
    }
}
