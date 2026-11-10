//! Static registry of providers and agents, including each agent's entitlement scopes.
//!
//! An *entitlement scope* names whose quota a piece of usage counts against
//! (architecture §6.1). Two agents share a scope only when provider documentation
//! confirms a shared entitlement. Every scope listed here is an initial assumption
//! until verified in M2 and recorded in `docs/providers/<provider>.md`.

use crate::ids::{AgentId, ProviderId};

#[derive(Debug, Clone, Copy)]
pub struct ProviderSpec {
    pub id: ProviderId,
    pub display_name: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct AgentSpec {
    pub agent: AgentId,
    pub provider: ProviderId,
    pub display_name: &'static str,
    /// Scope used when the collector has no reason to pick another.
    pub default_scope: &'static str,
    /// Every scope a collector for this agent may assign (e.g. subscription vs API-key mode).
    pub allowed_scopes: &'static [&'static str],
    /// Where the scope assumption comes from.
    pub scope_evidence: &'static str,
}

pub const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec { id: ProviderId::Anthropic, display_name: "Anthropic" },
    ProviderSpec { id: ProviderId::Openai, display_name: "OpenAI" },
    ProviderSpec { id: ProviderId::Google, display_name: "Google" },
    ProviderSpec { id: ProviderId::Mock, display_name: "Mock (dev only)" },
];

pub const AGENTS: &[AgentSpec] = &[
    AgentSpec {
        agent: AgentId::ClaudeCode,
        provider: ProviderId::Anthropic,
        display_name: "Claude Code",
        default_scope: "anthropic:claude_subscription",
        allowed_scopes: &["anthropic:claude_subscription", "anthropic:api"],
        scope_evidence: "verified M2: status line rate_limits are documented as the claude.ai plan windows, so claude.ai logins consume the plan; API-key use is anthropic:api (docs/providers/claude_code.md)",
    },
    AgentSpec {
        agent: AgentId::Codex,
        provider: ProviderId::Openai,
        display_name: "Codex",
        default_scope: "openai:chatgpt_codex",
        allowed_scopes: &["openai:chatgpt_codex", "openai:api"],
        scope_evidence: "verified M2: rollout rate_limits carry ChatGPT plan_type + 5h/7d windows (docs/providers/codex.md)",
    },
    AgentSpec {
        agent: AgentId::Antigravity,
        provider: ProviderId::Google,
        display_name: "Antigravity",
        default_scope: "google:antigravity",
        allowed_scopes: &["google:antigravity"],
        scope_evidence: "verified M6: Antigravity plan quotas are documented per model and separately from Gemini CLI; identity is install-scoped, one account per data folder (docs/providers/antigravity.md)",
    },
    AgentSpec {
        agent: AgentId::GeminiCli,
        provider: ProviderId::Google,
        display_name: "Gemini CLI",
        default_scope: "google:gemini_cli",
        allowed_scopes: &["google:gemini_cli", "google:gemini_api", "google:vertex"],
        scope_evidence: "verified M2/M5: scope follows settings.json security.auth.selectedType (oauth-personal → Code Assist, gemini-api-key, vertex-ai) (docs/providers/gemini_cli.md); kept separate from Antigravity",
    },
    AgentSpec {
        agent: AgentId::Mock,
        provider: ProviderId::Mock,
        display_name: "Mock collector",
        default_scope: "mock:dev",
        allowed_scopes: &["mock:dev"],
        scope_evidence: "synthetic dev-only data",
    },
];

pub fn agent_spec(agent: AgentId) -> &'static AgentSpec {
    // Every AgentId variant has exactly one entry (enforced by the test below).
    AGENTS
        .iter()
        .find(|s| s.agent == agent)
        .unwrap_or(&AGENTS[AGENTS.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_agent_has_one_spec_and_valid_default_scope() {
        for agent in [
            AgentId::ClaudeCode,
            AgentId::Codex,
            AgentId::Antigravity,
            AgentId::GeminiCli,
            AgentId::Mock,
        ] {
            let specs: Vec<_> = AGENTS.iter().filter(|s| s.agent == agent).collect();
            assert_eq!(specs.len(), 1, "{agent:?}");
            assert!(specs[0].allowed_scopes.contains(&specs[0].default_scope));
            assert!(PROVIDERS.iter().any(|p| p.id == specs[0].provider));
        }
    }

    #[test]
    fn antigravity_and_gemini_cli_never_share_a_scope() {
        let a = agent_spec(AgentId::Antigravity);
        let g = agent_spec(AgentId::GeminiCli);
        assert!(a.allowed_scopes.iter().all(|s| !g.allowed_scopes.contains(s)));
    }
}
