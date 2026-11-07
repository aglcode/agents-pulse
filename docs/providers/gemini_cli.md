# Gemini CLI: verified data sources

**Verified:** 2026-11-04 (M2) and 2026-11-07 (M5) against Gemini CLI **0.40.0** on Windows 11 (installed via npm under nvm). Auth: `security.auth.selectedType: oauth-personal`. Telemetry: **not configured**.

**How this was checked:**
- **M2:** settings keys and enums only.
- **M5** (approved 2026-11-07): field names, value types and counts from **one** session log and **one** subagent log. Only short enum values of `type`/`kind` and true/false comparisons were printed, plus token sums, which aren't content. No text, prompt or tool output was printed.
- **Never opened:** `~/.gemini/oauth_creds.json`, `tmp/*/logs.json`, `tmp/*/.project_root`.

## Sources, in priority order

| # | Source | Kind | Status | Gives |
|---|---|---|---|---|
| 1 | Session logs `~/.gemini/tmp/<project>/chats/*.jsonl` | Local file, **undocumented** | ✅ verified (M5), **used** | Per-reply tokens, model, session |
| 2 | `~/.gemini/google_accounts.json` → `active` | Local file, undocumented (not a credential) | ✅ verified, **used** | Identity (email) |
| 3 | `~/.gemini/settings.json` → `security.auth.selectedType` | Local file, documented setting | ✅ verified, **used** | Auth mode, which decides the scope |
| 4 | OpenTelemetry to a local outfile ([docs](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/telemetry.md)) | Official telemetry | ⏳ **detected only**: format unverified | Same API calls as #1 |
| — | Code Assist `retrieveUserQuota` | **Internal Google API**; needs the user's OAuth tokens from `oauth_creds.json` | ❌ **rejected** | Per-model `remainingFraction` / reset |

## Session logs (tokens, context)

**Layout:**
- `tmp/<project>/chats/session-<date>-<id prefix>.jsonl`: one session's log.
- `tmp/<project>/chats/<parent session id>/<id>.jsonl`: a subagent's log. Its header has `kind: "subagent"` and its **own** `sessionId` (the file name). The folder name equals the parent session's `sessionId` (verified).

**Line kinds** (all verified, append-only JSONL):

| Line | Fields | Used |
|---|---|---|
| Header (line 1) | `sessionId`, `projectHash`, `startTime`, `lastUpdated`, `kind` (`main` / `subagent`), `directories` (subagent) | `sessionId` and `kind` only |
| Message | `id`, `timestamp`, `type` (`user` / `gemini` / `info`), `content`, `model`, `tokens`, `thoughts`, `toolCalls` | `id`, `timestamp`, `type`, `model` and `tokens`, on `type: "gemini"` lines only |
| Update | `{"$set": {lastUpdated, summary?}}` | Ignored |

**Message lines:**
- Message lines carry no session id. It's read from the header, which AgentPulse reads separately whenever the file has changed.
- **A message is written again when it changes.** In both checked logs, every reply id appeared on 2 lines. The later line supersedes the earlier one.

**`tokens`** = `{input, cached, output, thoughts, tool, total}`. On every checked line, `cached ≤ input` and `total = input + output + thoughts + tool`. So `input` is the whole prompt (cached included), and `output` excludes thoughts, as in the Gemini API's `usageMetadata`.

**Mapping to AgentPulse's token semantics** (`core/src/usage.rs`):

| AgentPulse | From |
|---|---|
| `input` (fresh) | `input − cached` (saturating) `+ tool` |
| `cache_read` | `cached` |
| `cache_write` | not reported (`None`) |
| `output` (incl. reasoning) | `output + thoughts` |
| `reasoning` | `thoughts` |

**Keys:**
- **Dedup key:** `gemini:{hash(install, own sessionId)[..16]}:{message id}`. The session id is hashed, never stored in clear.
- **`source_seq`:** the line's byte offset, so a later rewrite replaces the record.
- **Session:**
  - A subagent's usage counts toward its parent session.
  - Session model and context follow main-thread replies only.
  - A log without a header stands in as its own session.

**Context:**
- **Used** = `tokens.input` of the session's latest main-thread reply (Local).
- **Total** comes from Google's published model limits, so it's **Estimated**: Gemini 2.0 / 2.5 / 3 / 3.1 Pro and Flash are 1,048,576, and 1.5 Pro is 2,097,152.
- **A model not in the table:** the total is unavailable, never guessed.

## Identity and entitlement
- **Identity:** `google_accounts.json` → `active`, a **VerifiedEmail**, used **only for `oauth-personal`**. The email is the account's display label; its identity key is hashed.
- **Scope, by `selectedType`** (the older top-level `selectedAuthType` is also read):
  - `oauth-personal` → `google:gemini_cli`
  - `gemini-api-key` → `google:gemini_api`, identity `AuthModeOnly`: the quota belongs to the key, not the Google account.
  - `vertex-ai` → `google:vertex`, `AuthModeOnly`.
  - Anything else → `google:gemini_cli`, identity `None` (install-scoped, never merged).
- **Kept separate from Antigravity**, even with the same Google email. They're different products with separately documented quotas.

## Telemetry: detected only
AgentPulse never changes Gemini CLI's telemetry setting (read-only rule, 2026-11-06). It reads `telemetry.enabled`, `target` and whether `outfile` is set, and shows the state on the Gemini CLI card. The outfile path is never kept.

The outfile is **not read** in M5:
- its format is undocumented and couldn't be verified (no real file exists, and producing one would mean changing the user's settings);
- it records the same API calls as the session logs, so using both would double-count;
- telemetry logs prompts by default (`logPrompts: true`).

Reading it is deferred until a real file the user produced can be verified. It would then only be used where session logs are absent.

## Subscription quota
**Unavailable** (`NotExposedByProvider`). The only source is the internal `retrieveUserQuota` call, which would mean reading `~/.gemini/oauth_creds.json` and calling a private API with the user's tokens. Both are forbidden by the project rules.

## Privacy
- **Only `chats/*.jsonl` and `chats/<parent>/*.jsonl` are opened** under `tmp/`. Nothing else is listed or read.
- **Parser:**
  - a byte prefilter drops every line without a `"tokens"` key before deserializing;
  - message lines are deserialized into a struct that names only `id`, `type`, `timestamp`, `model` and `tokens`;
  - `content`, `thoughts`, `toolCalls`, `projectHash` and `directories` are never named, so they're never kept.
- **Never read:** `~/.gemini/oauth_creds.json`, `tmp/*/logs.json` (prompt history), `tmp/*/.project_root` (a path).
- **Tests:**
  - sentinel privacy tests at batch level and end to end (DB and TRACE logs);
  - a byte-for-byte "home unchanged" test;
  - a stray usage-shaped log outside `chats/` must not be read.

## What the collector claims

| Metric | Source | Kind | Stability |
|---|---|---|---|
| Tokens by model and session | session logs | Local | Undocumented |
| Identity | `google_accounts.json` (Google login only) | Local | Undocumented |
| Context used / total | session logs + model table | used: Local / total: **Estimated** | Undocumented |
| Quota | — | **Unavailable** (`NotExposedByProvider`) | — |

## Risks
- The session-log format is undocumented and may change between Gemini CLI versions. Unknown lines are ignored, and malformed lines are counted, never logged.
- History exists only as far back as Gemini CLI keeps its session logs.
- Context totals are estimates. Gemini CLI may use a smaller effective window than the model's maximum.
