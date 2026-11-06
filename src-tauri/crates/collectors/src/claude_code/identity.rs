//! Who is signed in to one Claude Code config folder, read from Claude Code's own
//! settings file (`.claude.json`, undocumented). Read-only, and only `oauthAccount`
//! is ever deserialized; the rest of the file (project history, settings) is skipped
//! unread.
//!
//! `claude auth status --json` is deliberately **not** used: running the CLI writes
//! to the config folder (it creates `.claude.json` and rotates `backups/`), and
//! AgentPulse never changes a tool's files (docs/providers/claude_code.md).
//!
//! Never reads `.credentials.json`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agentpulse_core::identity::{IdentityEvidence, IdentityKind};
use agentpulse_core::secret::SecretString;
use serde::Deserialize;

/// Refuse to parse a settings file larger than this.
const MAX_FILE: u64 = 32 * 1024 * 1024;

pub(crate) const SUBSCRIPTION_SCOPE: &str = "anthropic:claude_subscription";
pub(crate) const API_SCOPE: &str = "anthropic:api";

#[derive(Deserialize)]
struct SettingsFile {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<OAuthAccount>,
}

#[derive(Deserialize)]
struct OAuthAccount {
    #[serde(rename = "accountUuid")]
    account_uuid: Option<SecretString>,
    #[serde(rename = "emailAddress")]
    email: Option<String>,
    #[serde(rename = "organizationUuid")]
    organization_uuid: Option<String>,
    #[serde(rename = "organizationType")]
    organization_type: Option<String>,
}

/// Identity plus the entitlement scope it implies.
#[derive(Debug, Clone)]
pub(crate) struct Signin {
    pub identity: IdentityEvidence,
    pub scope: &'static str,
    /// `oauthAccount.organizationType`, e.g. `claude_pro`, `claude_team`.
    pub org_type: Option<String>,
}

impl Signin {
    /// Whether AgentPulse supports this plan: individual claude.ai Pro, Max and Ultra
    /// (decided 2026-11-06). These are also the plans Claude Code's status line docs
    /// say report quota. Team, Enterprise and API-key folders are not read at all.
    pub fn plan_supported(&self) -> bool {
        matches!(self.org_type.as_deref(), Some("claude_pro" | "claude_max" | "claude_ultra"))
    }
}

/// Where Claude Code keeps the settings file for a config folder: `~/.claude.json`
/// for the default folder (no `CLAUDE_CONFIG_DIR`), otherwise inside the folder.
pub(crate) fn settings_files(config_dir: &Path, is_default: bool, user_home: &Path) -> Vec<PathBuf> {
    let inside = config_dir.join(".claude.json");
    if is_default { vec![user_home.join(".claude.json"), inside] } else { vec![inside] }
}

/// (path, mtime, size) of the first settings file that exists, for change detection.
pub(crate) fn fingerprint(files: &[PathBuf]) -> Option<(PathBuf, SystemTime, u64)> {
    files.iter().find_map(|p| {
        let m = std::fs::metadata(p).ok()?;
        Some((p.clone(), m.modified().ok()?, m.len()))
    })
}

/// Read the sign-in from the first settings file that has an `oauthAccount`.
pub(crate) fn read(files: &[PathBuf]) -> Signin {
    for path in files {
        let Ok(meta) = std::fs::metadata(path) else { continue };
        if meta.len() > MAX_FILE {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else { continue };
        if let Some(signin) = parse(&bytes) {
            return signin;
        }
    }
    signed_out()
}

pub(crate) fn parse(bytes: &[u8]) -> Option<Signin> {
    let file: SettingsFile = serde_json::from_slice(bytes).ok()?;
    let acct = file.oauth_account?;
    let uuid = acct.account_uuid?;
    let org_type = acct.organization_type.clone();
    let label = match (acct.email.as_deref(), plan_name(acct.organization_type.as_deref())) {
        (Some(e), Some(p)) => Some(format!("{e} ({p})")),
        (Some(e), None) => Some(e.to_owned()),
        (None, Some(p)) => Some(format!("Claude {p}")),
        (None, None) => None,
    };
    Some(Signin {
        identity: IdentityEvidence {
            kind: IdentityKind::VerifiedId,
            stable_value: Some(uuid),
            org: acct.organization_uuid.filter(|o| !o.is_empty()),
            display_label: label,
            evidence: ".claude.json oauthAccount.accountUuid (+ organizationUuid)".into(),
        },
        // A claude.ai login: Claude Code draws on the Pro/Max/Team plan (claude_code.md).
        scope: SUBSCRIPTION_SCOPE,
        org_type,
    })
}

/// No claude.ai login recorded: Claude Code is using an API key (or is signed out).
/// The account stays install-scoped under the API scope; nothing is merged.
pub(crate) fn signed_out() -> Signin {
    Signin {
        identity: IdentityEvidence::auth_mode_only("API key or signed out", ".claude.json has no oauthAccount"),
        scope: API_SCOPE,
        org_type: None,
    }
}

/// `claude_pro` → `Pro`, `claude_max` → `Max`, `claude_team` → `Team`.
pub(crate) fn plan_name(org_type: Option<&str>) -> Option<String> {
    let t = org_type?.strip_prefix("claude_")?;
    if t.is_empty() {
        return None;
    }
    let mut c = t.chars();
    let first = c.next()?;
    Some(first.to_uppercase().chain(c).collect::<String>().replace('_', " "))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn plan_names() {
        assert_eq!(plan_name(Some("claude_pro")).as_deref(), Some("Pro"));
        assert_eq!(plan_name(Some("claude_team")).as_deref(), Some("Team"));
        assert_eq!(plan_name(Some("claude_max")).as_deref(), Some("Max"));
        assert_eq!(plan_name(Some("other")), None);
        assert_eq!(plan_name(None), None);
    }
}
