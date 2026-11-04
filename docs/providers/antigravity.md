# Antigravity: verified data sources

**Verified:** 2026-11-04, **statically only**. Antigravity wasn't running, and no live call was made. Two installs are present:

| Install | Version | Modified |
|---|---|---|
| `%LOCALAPPDATA%\Programs\Antigravity IDE` | **2.5.5** (extension `antigravity` 0.2.0, VS Code base 1.107.0) | 2026-08-13 |
| `%LOCALAPPDATA%\Programs\Antigravity` | **2.0.11** (standalone app) | 2026-06-04 |

**How this was checked:** RPC method names and protobuf field names from the installed language server binary, plus daemon discovery files (`pid`, ports and `lsVersion` only). **`csrfToken` was never read.** `conversations/`, `brain/`, `knowledge/` and the `*.pb` state files were not opened.

## Sources

| # | Source | Kind | Status |
|---|---|---|---|
| — | Official docs ([plans](https://antigravity.google/docs/plans)) | Documentation | ✅ Quota is **per model**. It refreshes **every 5 hours** (Pro/Ultra) or **weekly** (other plans), with weekly rate limits, and is visible on the Settings page. **No programmatic interface is documented.** |
| 1 | Local language server RPC, `exa.language_server_pb.LanguageServerService/*` | **Undocumented local API** | ⚠️ Methods and fields exist in the 2.5.5 binary; ⏳ **discovery and response shape unverified** |

## What the 2.5.5 language server contains (static)
- **Read-only allowlist candidates:**
  - `GetUserStatus`
  - `GetModelStatuses`
  - `RetrieveUserQuota` / `…Summary`
  - `GetCascadeModelConfigData`
  - `GetAvailableModels`
  - `GetLocalUserInfo`
  - `GetUserAnalyticsSummary`
- **Must never be called:**
  - mutating methods: `Delete…`, `Set…`, `Send…`, `Install…`, `Cancel…`, `Revert…`
  - content-returning methods: `GetCascadeTrajectory…`, `GetUserTrajectory…`, `ConvertTrajectoryToMarkdown`, `GetCascadeMemories`, `GetUserMemories`
- **Embedded field names:** `quota_info`, `remaining_fraction`, `reset_time`, `model_or_alias`, `user_status`, `plan_info`, `plan_name`, `user_tier`, `teams_tier`, `available_prompt_credits`, `available_flow_credits`, `monthly_prompt_credits`, `max_tokens`.
- **Community tools** (not authoritative) read `GetUserStatus` → per-model `quotaInfo.{remainingFraction, resetTime}`, plus name, email and tier.

## ❗ Discovery finding: the architecture's assumption no longer holds
- **The daemon files are all stale.** Architecture §7 planned discovery via `~/.gemini/antigravity/daemon/ls_*.json`. All 5 files there, and the copies in `antigravity-ide/daemon` and `antigravity-backup/daemon`, date from **2026-02-26**, report `lsVersion 1.19.4`, and point at **PIDs that are no longer running**.
- **2.5.5 doesn't appear to write them.** The daemon files' process-identity check (live PID + executable path) correctly rejects every one of them.
- **The likely new mechanism is the process command line.** Community tools discover the server from it (`--app_data_dir antigravity`, with the CSRF token passed as an argument). That means AgentPulse would have to **read another process's command-line arguments to obtain a secret**. That's a more sensitive operation than reading a file, and must be weighed.

## Identity and entitlement
- **Identity:** most likely `GetUserStatus` (email) or `GetLocalUserInfo`. Until verified live, it's **install-scoped**, one per install (IDE 2.5.5 vs app 2.0.11).
- **Scope:** `google:antigravity`, **separate from Gemini CLI**, consistent with the docs describing Antigravity-specific plan quotas.

## Metrics: expected vs verified

| Metric | Expected source | Kind | Verified? |
|---|---|---|---|
| Quota per model (% remaining, reset time) | `GetUserStatus` → `quotaInfo` | Local | **Undocumented** · ⏳ |
| Plan / tier | `plan_info`, `user_tier` | Local | **Undocumented** · ⏳ |
| Tokens | none found (no per-request usage method identified) | — | **Unavailable** unless verified |
| Context | `max_tokens` model config only | Total: Local | **Undocumented** · ⏳; used: Unavailable |

`remaining_fraction` is "remaining", not "used". Convert to `used_pct = (1 − f) × 100` only as a labelled derivation. A missing fraction must be treated as **unknown, not full** (community tools assume full, which would fabricate data).

## What's needed before M3 can target Antigravity
1. Antigravity IDE running.
2. **Your approval** to:
   - (a) locate the server by its process (executable path inside the install directory),
   - (b) read the CSRF token from that process's arguments into memory only,
   - (c) make **one** loopback call to `GetUserStatus` and record only its **response shape** (key paths and types, no values).
3. A version table entry for the verified build. Any other build gets a strict-schema probe or `UnsupportedVersion`.

## Risks
- **Fully undocumented API.** Google changed discovery between 1.19 and 2.5 without notice; it can change again.
- **Obtaining the CSRF token requires reading another process's command line.**
- Local HTTPS uses a self-signed certificate (`dist/languageServer/cert.pem`), so the client must pin to loopback and this port only.
- **No token usage source** has been identified.
