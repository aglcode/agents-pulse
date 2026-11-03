//! Identifiers for providers (companies), agents (applications) and collectors.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The company that owns an account and its entitlements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ProviderId {
    Anthropic,
    Openai,
    Google,
    /// Synthetic provider used only by dev-build mock collectors.
    Mock,
}

impl ProviderId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Google => "google",
            Self::Mock => "mock",
        }
    }
}

/// An application that consumes a provider entitlement. Agents are not accounts (blueprint §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentId {
    ClaudeCode,
    Codex,
    Antigravity,
    GeminiCli,
    /// Synthetic agent used only by dev-build mock collectors.
    Mock,
}

impl AgentId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
            Self::GeminiCli => "gemini_cli",
            Self::Mock => "mock",
        }
    }
}

/// Stable identifier of a collector instance, e.g. `"codex"` or `"mock-healthy"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CollectorId(pub String);

impl CollectorId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CollectorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
