//! Provider collectors. Each provider lives in its own module and implements
//! [`agentpulse_core::collector::Collector`]. Verified data sources per provider
//! are documented in `docs/providers/`.

pub mod codex;
#[cfg(feature = "mock")]
pub mod mock;
