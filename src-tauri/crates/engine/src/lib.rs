//! The monitoring engine: supervises collectors, resolves accounts, submits
//! batches to the writer and publishes change events (architecture §3).
//!
//! The engine never creates a Tokio runtime. It runs on the handle it is given,
//! which in the app is Tauri's own runtime.

pub mod bus;
pub mod resolver;
mod supervisor;

pub use bus::EngineEvent;
pub use supervisor::{CollectorInfo, Engine, EngineConfig, EngineError, ShutdownReport, ENABLED_SETTING};
