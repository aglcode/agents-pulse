//! Provider collectors. Each provider lives in its own module and implements
//! [`agentpulse_core::collector::Collector`]. Verified data sources per provider
//! are documented in `docs/providers/`.

pub mod antigravity;
pub mod claude_code;
pub mod codex;
pub mod gemini_cli;
mod jsonl_tail;
#[cfg(feature = "mock")]
pub mod mock;
