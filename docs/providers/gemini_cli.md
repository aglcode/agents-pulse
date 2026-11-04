# Gemini CLI: verified data sources

**Verified:** 2026-11-04 against Gemini CLI **0.40.0** on Windows 11 (installed via npm under nvm). Auth: `security.auth.selectedType: oauth-personal`. Telemetry: **not configured**.

**How this was checked:** settings keys and enums only. **No chat, log, or `~/.gemini/tmp/` file was read, and `~/.gemini/oauth_creds.json` was not opened.**

## Sources, in blueprint priority order

| # | Source | Kind | Status | Gives |
|---|---|---|---|---|
| 1 | OpenTelemetry to a local **outfile** ([docs](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/telemetry.md)) | Official telemetry | ✅ documented; ⏳ not enabled or exercised | Per-request tokens, model, session, `user.email` |
| 2 | `~/.gemini/google_accounts.json` → `active` | Local file, undocumented (not a credential) | ✅ verified (key names) | Identity (email) without telemetry |
| 3 | `~/.gemini/settings.json` → `security.auth.selectedType` | Local file, documented setting | ✅ verified | Auth mode, which decides the scope |
| — | Code Assist `retrieveUserQuota` | **Internal Google API**; needs the user's OAuth tokens from `oauth_creds.json` | ❌ **rejected** | Per-model `remainingFraction` / reset |

## Identity and entitlement
- **Identity:** telemetry `user.email` (official, when telemetry is on); otherwise `google_accounts.json` → `active`. Both are **VerifiedEmail**.
- **Scope, by `selectedType`:**
  - `oauth-personal` → `google:gemini_cli`
  - `gemini-api-key` → `google:gemini_api`
  - `vertex-ai` → `google:vertex`
- **Kept separate from Antigravity**, even with the same Google email. They're different products with separately documented quotas (Code Assist buckets vs Antigravity plan quotas).

## Usage (tokens), telemetry only
- **Setup:** `settings.json` → `telemetry: { enabled: true, target: "local", outfile: <AgentPulse-owned path>, logPrompts: false }`.
- **Event:** `gemini_cli.api_response` → `model`, `input_token_count`, `output_token_count`, `cached_content_token_count`, `thoughts_token_count`, `tool_token_count`, `total_token_count`, `prompt_id`, `duration_ms`, `auth_type`, with common attributes `session.id` and `installation.id`.
- **Dedup key:** `gemini:{session.id}:{prompt_id}:{event sequence}`.
  - ⏳ `prompt_id` covers a whole prompt, which can involve several API calls. Confirm against a real outfile which attribute is unique per response before M4.
- **Outfile format:** not documented, so it must be verified against a real file.

## Subscription quota
**Unavailable.** The only source is the internal `retrieveUserQuota` call. Using it would mean reading `~/.gemini/oauth_creds.json` and calling a private API with the user's tokens, both forbidden by the project rules. It's shown as `Unavailable: NotExposedByProvider`.

## Context window
Not in telemetry. Used = `input_token_count` of the latest `api_response` in a session. Total comes from a model table, so it's **Estimated**.

## Privacy
- **Telemetry logs prompts by default (`logPrompts: true`)**, and `api_response` may include `response_text`. AgentPulse must:
  - set `logPrompts: false`;
  - make its parser ignore text fields;
  - delete or rotate its own outfile after reading, because Gemini CLI writes it.
- **Never read:** `~/.gemini/oauth_creds.json`, or `~/.gemini/tmp/**` (chats and `logs.json` contain prompts).

## What the collector would claim

| Metric | Source | Kind | Stability |
|---|---|---|---|
| Tokens by model and session | telemetry outfile (opt-in config change) | Official telemetry | Documented |
| Identity | telemetry `user.email` / `google_accounts.json` | Official telemetry / Local | Documented / Undocumented |
| Context used / total | telemetry + model table | used: Official telemetry / total: **Estimated** | Documented |
| Quota | — | **Unavailable** (`NotExposedByProvider`) | — |
| Anything before telemetry is enabled | — | **Unavailable** (`OptInRequired`) | — |

## Risks
- Requires editing the user's `~/.gemini/settings.json`. It must be explicit, minimal (only the `telemetry` block), and reversible on Disable.
- No history: usage only exists from the moment telemetry is enabled.
- The outfile format is undocumented and may change (OTel SDK JSON).
