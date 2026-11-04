//! Provider collectors. Each provider lives in its own module and implements
//! [`agentpulse_core::collector::Collector`]. Real providers arrive from M3,
//! after the M2 verification spike.

#[cfg(feature = "mock")]
pub mod mock;
