//! Engine → app events. Payloads are identifiers only; the UI refetches the slices
//! that changed.

/// Bounded so a stalled consumer can't grow memory. Lagging receivers resync.
pub const BUS_CAPACITY: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    /// A batch changed data for this account.
    DataChanged { collector_id: String, usage_account_id: i64 },
    /// A collector changed state (failure, recovery, not installed, ...).
    CollectorStatusChanged { collector_id: String },
}
