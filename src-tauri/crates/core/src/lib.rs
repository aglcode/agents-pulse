//! Domain types shared by every AgentPulse crate.
//!
//! Nothing in here performs I/O. The crate defines the normalized vocabulary
//! (metrics, sources, identities, batches) and the [`collector::Collector`]
//! trait that every provider integration implements.

pub mod batch;
pub mod collector;
pub mod dto;
pub mod hash;
pub mod identity;
pub mod ids;
pub mod metric;
pub mod registry;
pub mod secret;
pub mod time;
pub mod usage;
