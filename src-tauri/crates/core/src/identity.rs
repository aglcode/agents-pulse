//! Identity (who is signed in) vs usage account (whose entitlement the usage counts against).
//! See architecture §6.1.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::ProviderId;
use crate::secret::SecretString;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum IdentityKind {
    /// A provider-issued stable account id (e.g. Anthropic `accountUuid`).
    VerifiedId,
    /// A provider-reported email address (less stable than an id; never auto-merged across agents).
    VerifiedEmail,
    /// Only the authentication mode is known (e.g. "ChatGPT login"), with no identity.
    AuthModeOnly,
    /// Nothing is known.
    None,
}

impl IdentityKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VerifiedId => "verified_id",
            Self::VerifiedEmail => "verified_email",
            Self::AuthModeOnly => "auth_mode_only",
            Self::None => "none",
        }
    }

    pub const fn is_verified(self) -> bool {
        matches!(self, Self::VerifiedId | Self::VerifiedEmail)
    }
}

/// What a collector learned about who is signed in to one installation.
///
/// `stable_value` is held as a secret so it never reaches logs or `Debug` output.
/// It is only ever exposed to hash it into an identity key.
#[derive(Debug, Clone)]
pub struct IdentityEvidence {
    pub kind: IdentityKind,
    pub stable_value: Option<SecretString>,
    /// Optional organization id (seat quotas can be per organization).
    pub org: Option<String>,
    /// Human-readable label kept locally (e.g. an email address or "ChatGPT login").
    pub display_label: Option<String>,
    /// Where the evidence came from, e.g. `"~/.claude.json oauthAccount.accountUuid"`.
    pub evidence: String,
}

impl IdentityEvidence {
    pub fn none(evidence: impl Into<String>) -> Self {
        Self { kind: IdentityKind::None, stable_value: None, org: None, display_label: None, evidence: evidence.into() }
    }

    pub fn auth_mode_only(label: impl Into<String>, evidence: impl Into<String>) -> Self {
        Self {
            kind: IdentityKind::AuthModeOnly,
            stable_value: None,
            org: None,
            display_label: Some(label.into()),
            evidence: evidence.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AccountKind {
    /// Keyed by a verified identity within one entitlement scope.
    Verified,
    /// No verified identity: keyed by agent + installation.
    InstallScoped,
}

impl AccountKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::InstallScoped => "install_scoped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIdentity {
    pub kind: IdentityKind,
    /// `stable_hash(provider, normalized stable value)`.
    pub identity_key: String,
    pub display_label: Option<String>,
}

/// Output of the engine's resolver; the store upserts exactly this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountResolution {
    pub provider: ProviderId,
    pub entitlement_scope: String,
    pub account_key: String,
    pub account_kind: AccountKind,
    pub identity: Option<ResolvedIdentity>,
    pub default_label: String,
}
