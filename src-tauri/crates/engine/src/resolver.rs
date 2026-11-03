//! Identity + entitlement resolution (architecture §6.1).
//!
//! * The account key always includes the entitlement scope, so one identity
//!   under two scopes (e.g. Antigravity and Gemini CLI on the same Google login)
//!   gives two usage accounts that share one identity row.
//! * The agent is *not* part of a verified account key, so agents that share a
//!   documented scope and a verified identity share one account (no duplicates).
//! * Without a verified identity the account is install-scoped and never merges
//!   automatically.

use agentpulse_core::batch::NormalizedBatch;
use agentpulse_core::hash::stable_hash;
use agentpulse_core::identity::{AccountKind, AccountResolution, IdentityKind, ResolvedIdentity};
use agentpulse_core::registry::agent_spec;
use agentpulse_core::secret::{ExposeSecret, Zeroizing};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("entitlement scope {scope} is not allowed for agent {agent}")]
    ScopeNotAllowed { agent: &'static str, scope: String },
}

pub fn resolve(batch: &NormalizedBatch) -> Result<AccountResolution, ResolveError> {
    let spec = agent_spec(batch.agent);
    let scope = batch.entitlement_scope.as_str();
    if !spec.allowed_scopes.contains(&scope) {
        return Err(ResolveError::ScopeNotAllowed { agent: batch.agent.as_str(), scope: scope.to_owned() });
    }
    let provider = spec.provider;
    let id = &batch.identity;

    if id.kind.is_verified()
        && let Some(secret) = &id.stable_value
    {
        // The normalized copy of the identity value is zeroed when it goes out of scope.
        let normalized = Zeroizing::new(normalize(id.kind, secret.expose_secret()));
        if !normalized.is_empty() {
            let identity_key = stable_hash(&[provider.as_str(), id.kind.as_str(), normalized.as_str()]);
            let account_key =
                stable_hash(&["verified", scope, &identity_key, id.org.as_deref().unwrap_or("")]);
            let default_label = match &id.display_label {
                Some(l) => format!("{} · {l}", spec.display_name),
                None => spec.display_name.to_owned(),
            };
            return Ok(AccountResolution {
                provider,
                entitlement_scope: scope.to_owned(),
                account_key,
                account_kind: AccountKind::Verified,
                identity: Some(ResolvedIdentity {
                    kind: id.kind,
                    identity_key,
                    display_label: id.display_label.clone(),
                }),
                default_label,
            });
        }
    }

    let account_key = stable_hash(&["install", scope, batch.agent.as_str(), &batch.install_key]);
    let default_label = match (id.kind, &id.display_label) {
        (IdentityKind::AuthModeOnly, Some(l)) => format!("{} ({l})", spec.display_name),
        _ => format!("{} (this device)", spec.display_name),
    };
    Ok(AccountResolution {
        provider,
        entitlement_scope: scope.to_owned(),
        account_key,
        account_kind: AccountKind::InstallScoped,
        identity: None,
        default_label,
    })
}

fn normalize(kind: IdentityKind, value: &str) -> String {
    let t = value.trim();
    match kind {
        IdentityKind::VerifiedEmail => t.to_lowercase(),
        _ => t.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use agentpulse_core::identity::IdentityEvidence;
    use agentpulse_core::ids::{AgentId, CollectorId};
    use agentpulse_core::secret::SecretString;

    fn batch(agent: AgentId, scope: &str, install: &str, identity: IdentityEvidence) -> NormalizedBatch {
        NormalizedBatch {
            collector_id: CollectorId::new("t"),
            agent,
            entitlement_scope: scope.into(),
            install_key: install.into(),
            identity,
            detected_version: None,
            collected_at: 0,
            sessions: vec![],
            usage: vec![],
            quotas: vec![],
            contexts: vec![],
            availability: vec![],
            cursors: vec![],
        }
    }

    fn email(e: &str) -> IdentityEvidence {
        IdentityEvidence {
            kind: IdentityKind::VerifiedEmail,
            stable_value: Some(SecretString::from(e)),
            org: None,
            display_label: Some(e.into()),
            evidence: "test".into(),
        }
    }

    #[test]
    fn same_google_identity_under_antigravity_and_gemini_cli_gives_separate_accounts_one_identity() {
        let a = resolve(&batch(AgentId::Antigravity, "google:antigravity", "i1", email("me@example.com"))).unwrap();
        let g = resolve(&batch(AgentId::GeminiCli, "google:gemini_cli", "i2", email("me@example.com"))).unwrap();
        assert_ne!(a.account_key, g.account_key, "usage and quota must stay separate");
        assert_eq!(
            a.identity.as_ref().unwrap().identity_key,
            g.identity.as_ref().unwrap().identity_key,
            "both reference the same identity"
        );
    }

    #[test]
    fn same_scope_and_verified_identity_gives_one_account_across_installs() {
        let a = resolve(&batch(AgentId::GeminiCli, "google:gemini_cli", "laptop", email("Me@Example.com "))).unwrap();
        let b = resolve(&batch(AgentId::GeminiCli, "google:gemini_cli", "desktop", email("me@example.com"))).unwrap();
        assert_eq!(a.account_key, b.account_key);
        assert_eq!(a.account_kind, AccountKind::Verified);
    }

    #[test]
    fn unverified_identities_are_install_scoped_and_never_merge() {
        let a = resolve(&batch(AgentId::Codex, "openai:chatgpt_codex", "home1", IdentityEvidence::auth_mode_only("ChatGPT login", "codex login status"))).unwrap();
        let b = resolve(&batch(AgentId::Codex, "openai:chatgpt_codex", "home2", IdentityEvidence::auth_mode_only("ChatGPT login", "codex login status"))).unwrap();
        assert_eq!(a.account_kind, AccountKind::InstallScoped);
        assert!(a.identity.is_none());
        assert_ne!(a.account_key, b.account_key);
        assert_eq!(a.default_label, "Codex (ChatGPT login)");
    }

    #[test]
    fn verified_kind_without_a_value_falls_back_to_install_scoped() {
        let mut ev = email("x@example.com");
        ev.stable_value = None;
        let r = resolve(&batch(AgentId::GeminiCli, "google:gemini_cli", "i", ev)).unwrap();
        assert_eq!(r.account_kind, AccountKind::InstallScoped);
    }

    #[test]
    fn org_separates_seat_accounts_for_the_same_user() {
        let mut personal = email("me@example.com");
        personal.kind = IdentityKind::VerifiedId;
        let mut seat = personal.clone();
        seat.org = Some("org-1".into());
        let a = resolve(&batch(AgentId::ClaudeCode, "anthropic:claude_subscription", "i", personal)).unwrap();
        let b = resolve(&batch(AgentId::ClaudeCode, "anthropic:claude_subscription", "i", seat)).unwrap();
        assert_ne!(a.account_key, b.account_key);
    }

    #[test]
    fn disallowed_scope_is_rejected() {
        let r = resolve(&batch(AgentId::Antigravity, "google:gemini_cli", "i", email("a@b.c")));
        assert!(matches!(r, Err(ResolveError::ScopeNotAllowed { .. })));
    }

    #[test]
    fn identity_value_never_appears_in_resolution_or_debug_output() {
        let ev = IdentityEvidence {
            kind: IdentityKind::VerifiedId,
            stable_value: Some(SecretString::from("uuid-SENTINEL-1234")),
            org: None,
            display_label: None,
            evidence: "test".into(),
        };
        let b = batch(AgentId::ClaudeCode, "anthropic:claude_subscription", "i", ev);
        assert!(!format!("{b:?}").contains("SENTINEL"));
        let r = resolve(&b).unwrap();
        assert!(!format!("{r:?}").contains("SENTINEL"));
    }
}
