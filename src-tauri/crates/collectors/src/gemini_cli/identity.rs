//! Who is signed in to Gemini CLI, how, and whether telemetry is on. Read-only.
//!
//! * `settings.json` (documented settings): only `security.auth.selectedType` (or the
//!   older top-level `selectedAuthType`) and `telemetry.{enabled,target,outfile}` are
//!   deserialized. The outfile path itself is never kept, only whether one is set.
//! * `google_accounts.json` (undocumented, not a credential): only `active`, the
//!   signed-in Google account's email.
//!
//! Never reads `oauth_creds.json`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agentpulse_core::identity::{IdentityEvidence, IdentityKind};
use agentpulse_core::secret::SecretString;
use serde::Deserialize;
use serde::de::IgnoredAny;

/// Refuse to parse either file if it's larger than this (both are tiny).
const MAX_FILE: u64 = 1024 * 1024;

pub(crate) const CODE_ASSIST_SCOPE: &str = "google:gemini_cli";
pub(crate) const API_SCOPE: &str = "google:gemini_api";
pub(crate) const VERTEX_SCOPE: &str = "google:vertex";

#[derive(Deserialize)]
struct Settings {
    security: Option<Security>,
    /// Gemini CLI before the nested settings format.
    #[serde(rename = "selectedAuthType")]
    legacy_auth_type: Option<String>,
    telemetry: Option<Telemetry>,
}

#[derive(Deserialize)]
struct Security {
    auth: Option<Auth>,
}

#[derive(Deserialize)]
struct Auth {
    #[serde(rename = "selectedType")]
    selected_type: Option<String>,
}

#[derive(Deserialize)]
struct Telemetry {
    enabled: Option<bool>,
    target: Option<String>,
    /// Only its presence matters; the path is never kept.
    outfile: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct GoogleAccounts {
    active: Option<String>,
}

/// Gemini CLI's own telemetry setting, as the user configured it. AgentPulse never
/// changes it and doesn't read the telemetry file yet (docs/providers/gemini_cli.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryState {
    Off,
    /// On, writing to a local file.
    LocalFile,
    /// On, sent somewhere else (e.g. Google Cloud).
    Other,
}

/// Identity plus the entitlement scope it implies.
#[derive(Debug, Clone)]
pub(crate) struct Signin {
    pub identity: IdentityEvidence,
    pub scope: &'static str,
    pub telemetry: TelemetryState,
}

pub(crate) fn settings_path(home: &Path) -> PathBuf {
    home.join("settings.json")
}

pub(crate) fn accounts_path(home: &Path) -> PathBuf {
    home.join("google_accounts.json")
}

/// (mtime, size) of both files, for change detection.
pub(crate) type Fingerprint = [Option<(SystemTime, u64)>; 2];

pub(crate) fn fingerprint(home: &Path) -> Fingerprint {
    let f = |p: PathBuf| std::fs::metadata(p).ok().and_then(|m| Some((m.modified().ok()?, m.len())));
    [f(settings_path(home)), f(accounts_path(home))]
}

pub(crate) fn read(home: &Path) -> Signin {
    let settings = read_small(&settings_path(home)).and_then(|b| serde_json::from_slice::<Settings>(&b).ok());
    let auth_type = settings.as_ref().and_then(|s| {
        s.security
            .as_ref()
            .and_then(|sec| sec.auth.as_ref())
            .and_then(|a| a.selected_type.clone())
            .or_else(|| s.legacy_auth_type.clone())
    });
    let telemetry = match settings.as_ref().and_then(|s| s.telemetry.as_ref()) {
        Some(t) if t.enabled == Some(true) => {
            if t.target.as_deref().is_none_or(|x| x == "local") && t.outfile.is_some() {
                TelemetryState::LocalFile
            } else {
                TelemetryState::Other
            }
        }
        _ => TelemetryState::Off,
    };
    let active = read_small(&accounts_path(home))
        .and_then(|b| serde_json::from_slice::<GoogleAccounts>(&b).ok())
        .and_then(|a| a.active)
        .map(|e| e.trim().to_owned())
        .filter(|e| e.contains('@'));
    let (identity, scope) = identify(auth_type.as_deref(), active);
    Signin { identity, scope, telemetry }
}

/// Scope by auth type (docs/providers/gemini_cli.md). Only a Google login
/// (`oauth-personal`) is tied to the Google account in `google_accounts.json`; with
/// an API key or Vertex AI the quota belongs to the key or project, so the account
/// stays install-scoped and is never merged by email.
fn identify(auth_type: Option<&str>, active: Option<String>) -> (IdentityEvidence, &'static str) {
    match auth_type {
        Some("oauth-personal") => match active {
            Some(email) => (
                IdentityEvidence {
                    kind: IdentityKind::VerifiedEmail,
                    stable_value: Some(SecretString::from(email.to_lowercase())),
                    org: None,
                    display_label: Some(email),
                    evidence: "google_accounts.json active (settings.json security.auth.selectedType = oauth-personal)"
                        .into(),
                },
                CODE_ASSIST_SCOPE,
            ),
            None => (
                IdentityEvidence::auth_mode_only("Google login", "google_accounts.json has no active account"),
                CODE_ASSIST_SCOPE,
            ),
        },
        Some("gemini-api-key") => {
            (IdentityEvidence::auth_mode_only("Gemini API key", "settings.json security.auth.selectedType"), API_SCOPE)
        }
        Some("vertex-ai") => {
            (IdentityEvidence::auth_mode_only("Vertex AI", "settings.json security.auth.selectedType"), VERTEX_SCOPE)
        }
        // Other or no auth type: nothing verified. Install-scoped under Gemini CLI's
        // default scope, never merged with anything.
        _ => (IdentityEvidence::none("settings.json has no recognised auth type"), CODE_ASSIST_SCOPE),
    }
}

fn read_small(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return None;
    }
    std::fs::read(path).ok()
}
