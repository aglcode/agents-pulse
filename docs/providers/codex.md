# Codex: verified data sources

**Verified:** 2026-11-04 against `codex-cli` **0.157.0** on Windows 11. The rollout logs also contain sessions written by **newer clients (up to 0.160.0)**, likely the Codex desktop app sharing `~/.codex`. Auth: `codex login status` → "Logged in using ChatGPT".

**How this was checked:** event types, key paths and value types only. String values and conversation content were never printed. **`~/.codex/auth.json` was not opened.**

## Sources, in blueprint priority order

| # | Source | Kind | Status | Gives |
|---|---|---|---|---|
| 1 | **`codex app-server`** JSON-RPC: `account/read`, `account/rateLimits/read`, `account/usage/read`, notification `account/rateLimits/updated` | Official CLI interface. The command is labelled `[experimental]`; its README says the schema is **unstable** | ✅ methods and schemas verified from `codex app-server generate-json-schema` (stable set, not `--experimental`); ⏳ **not called live** | **Identity (email, plan)**, **live rate limits**, daily token buckets |
| 2 | OpenTelemetry `[otel]` in `config.toml` ([docs](https://learn.chatgpt.com/docs/config-file/config-advanced)) | Official telemetry | ✅ documented | Token counts on `codex.sse_event` (`response.completed`) only; **no rate limits** |
| 3 | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl.zst` | Local file, **undocumented**, zstd-compressed | ✅ verified locally (61 files) | Per-response tokens, **rate limits**, **context window**, model, session |

## Identity and entitlement
- **Identity, preferred:** `account/read` → `account` is one of:
  - `ChatgptAccount { type, email, planType }`
  - `ApiKeyAccount`
  - `AmazonBedrockAccount`

  With `refreshToken: false` it shouldn't touch tokens. This is a **supported** interface that gives **VerifiedEmail**, so the Codex account no longer has to be install-scoped. It replaces the MVP decision from architecture §6.3 if you approve it.
- **Identity, fallback:** the rollout `session_meta.payload.creator_account_id` (undocumented local field). `account/rateLimits/read` also returns `accountId`.
- **Never:** `~/.codex/auth.json`.
- **Scope:**
  - `ChatgptAccount` → `openai:chatgpt_codex`.
  - `ApiKeyAccount` → `openai:api`.
  - Bedrock → out of scope (not supported).
  - `planType` values: `free|go|plus|pro|prolite|team|business|enterprise|edu…` (from the schema).

## Usage (tokens)

**Per response, preferred:** `token_usage_record` lines, present in CLI **≥ 0.153** (51 of 61 files).

| Field | Path |
|---|---|
| Dedup id | `payload.response_id` (4,620 seen; **none repeated across files**) |
| Tokens | `payload.usage.{input_tokens, cached_input_tokens, cache_write_input_tokens, output_tokens, reasoning_output_tokens, total_tokens}` |
| Session | `payload.session_id`, `payload.thread_id`, `payload.turn_id` |
| Time / order | `timestamp`, `ordinal` |

- **Dedup key:** `codex:{response_id}`.
- **`source_seq`:** `ordinal`.

**Fallback, older files:** `event_msg/token_count` → `payload.info.last_token_usage.*` per event.
- **Dedup key:** `codex:{session_id}:tc:{ordinal}`.
- **Rule:** **per file, use records if present, otherwise `token_count`. Never both**, or usage is double-counted.
- ⏳ Some 0.157–0.158 files have `token_count` but no records. Before M6, confirm whether those are rate-limit-only events (`info` null).

**Model:** `turn_context.payload.model`, per turn.

## Subscription quota
- **Rollouts:** every `token_count` event (530 of 530 sampled) carries `payload.rate_limits`:
  - `primary.{used_percent, window_minutes, resets_at}` (observed: 300 min, i.e. 5 hours)
  - `secondary.*` (observed: 10,080 min, i.e. 7 days)
  - plus `plan_type`, `limit_id`, `credits.{has_credits, unlimited, balance}`, `rate_limit_reached_type`
- **app-server:** `account/rateLimits/read` → `RateLimitSnapshot { primary/secondary: RateLimitWindow { usedPercent, windowDurationMins, resetsAt }, planType, credits, … }`, plus `rateLimitsByLimitId`.
- **Freshness:** rollout values are only as fresh as the last Codex activity. After `resets_at` passes, the value is **stale/unknown, not 0%**. app-server gives live values, but **contacts OpenAI's backend through the user's own Codex client** on each call. Poll it rarely (≥ 5 min) and only while enabled.
- **Window kind:** from `window_minutes` (`300` → `5h`, `10080` → `7d`, otherwise `{n}m`). Don't hard-code primary as 5h.

## Context window
- `token_count.payload.info.model_context_window` (observed `258400`).
- Used = `last_token_usage.input_tokens`. Both are local observations; the percentage is computed.

## Privacy
- **Never read:** `~/.codex/auth.json`.
- **Content-bearing lines to skip entirely:** `response_item/*` (message, reasoning, function and custom tool calls with arguments and **outputs**), `compacted`, `world_state`, and `turn_context` fields such as `cwd`, `summary` and `developer_instructions`.
- **OTel:** `codex.tool_result` includes an **output snippet**. Any OTel consumer must drop it, and must never set `log_user_prompt` or `log_agent_responses`.

## What the collector would claim

| Metric | Source | Kind | Stability |
|---|---|---|---|
| Tokens by model and session | rollouts (`token_usage_record` / `token_count`) | Local | Undocumented |
| Quota primary / secondary | rollouts (last seen) | Local | Undocumented |
| Quota (live) | app-server `account/rateLimits/read` (if approved) | Official API | Documented, unstable schema |
| Context used / total | rollouts `token_count.info` | Local | Undocumented |
| Identity (email, plan) | app-server `account/read` (if approved) | Official CLI | Documented, unstable schema |
| Identity (no app-server) | install-scoped + label | — | — |

## Risks
- Two undocumented, fast-moving formats: record types appeared mid-2026, and multiple client versions write to the same directory. The parser must be version-tolerant and gated by `cli_version`.
- The app-server protocol is explicitly unstable. Gate it by `codex --version` using the generated schema, and fall back to rollouts.
- Rollouts are zstd-compressed, so the collector needs the `zstd` crate (already planned).

## Implementation (M3, 2026-11-04)
Code: `src-tauri/crates/collectors/src/codex/`. **Local logs only:** no config changes, no network, no subprocesses. `codex app-server` is deferred until you decide on it.

- **Files:**
  - Reads `sessions/**` and `archived_sessions/**` `rollout-*.jsonl[.zst]` under `$CODEX_HOME` (default `~/.codex`).
  - Each file has a cursor keyed by the hash of its relative path. Unchanged `.zst` files are skipped.
  - A plain `.jsonl` file still being written is re-scanned for context, but only lines past the cursor are emitted, and a partial last line is never consumed.
- **Triggers:** a recursive file watcher on both folders (2 s debounce), plus a 5-minute fallback poll.
- **Parser:** only `session_meta`, `turn_context`, `token_usage_record` and `event_msg/token_count` payloads are ever deserialized, into usage-only structs. Every other line type stays an undeserialized raw slice.
- **Tokens:**
  - Mapped to AgentPulse semantics: `input` = `input_tokens − cached_input_tokens`, `cache_read` = `cached_input_tokens`, `output` = `output_tokens` (reasoning included), `reasoning` = `reasoning_output_tokens`.
  - CLI ≥ 0.153 uses `token_usage_record` only. Older files use deltas of `total_token_usage`, so a repeated `token_count` adds nothing.
- **Quota:**
  - Window kind comes from `window_minutes` (`5h`, `7d`, `30d`…), prefixed by `limit_id` unless it's the main `codex` limit (e.g. `premium:5h`).
  - Readings are sorted by time and de-duplicated: a change is kept, plus the final reading, which extends the current segment.
- **Presentation rules** (store, apply to every provider):
  - A window missing from its limit's newest snapshot is hidden. For example, a `30d` window from a previous free plan.
  - A window whose `resets_at` has passed shows **"Window reset since the last reading"** instead of its old percentage.
- **Identity:** **install-scoped** account, labelled from the latest `rate_limits.plan_type` (e.g. "Codex (ChatGPT Plus)"), as `AuthModeOnly`. There's no verified identity until `app-server` is approved.
- **Real-data check (this machine):** 65 rollout files → 65 sessions, 4,862 usage records, quota windows `5h` / `7d`, label "ChatGPT Plus", latest client 0.160.0, batch valid. The quota readings are from 2026-10-04, so the app shows them as reset rather than current.
