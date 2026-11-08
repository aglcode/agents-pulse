# Antigravity: verified data sources

**Verified:**
- **2026-11-04 (M2), statically:** the installed language server's RPC methods and protobuf field names, plus daemon discovery files (`pid`, ports and `lsVersion` only).
- **2026-11-07 (M6 research), local files only:** the conversation stores' formats and the protobuf definitions they use. Approved step by step.

**How the 2026-11-07 checks were done:**
- **Entirely local:** no network, no calls to Antigravity's language server.
- **Antigravity's files were never opened for writing:** one `.db` was copied to a scratch folder and only the copy was opened. Each original was read through a handle that told Windows not to update its last-access time.
- **Nothing changed:** before/after snapshots of size, created, modified and last-access times were identical for `~/.gemini/antigravity-ide` (4,202 entries) and the install folder (47,916 entries). Scratch copies were deleted.
- **Printed:** table and column names, declared types, row counts, protobuf field numbers and wire types, string lengths, entropy, and the protobuf definitions compiled into the language server.
- **Never printed or kept:** any value, text, prompt or ID. `csrfToken` has never been read.

## Installs

| Install | Version | Data folder | Conversations |
|---|---|---|---|
| `%LOCALAPPDATA%\Programs\Antigravity IDE` | **2.5.5** (extension `antigravity` 0.2.0) | `~/.gemini/antigravity-ide` | 20 `.db` (SQLite) + 102 `.pb`, newest 2026-09-05 |
| `%LOCALAPPDATA%\Programs\Antigravity` | **2.0.11** (standalone app) | `~/.gemini/antigravity` | 100 `.pb` + 1 `.db`, newest 2026-05-23 |
| — | — | `~/.gemini/antigravity-backup` | A copy of `antigravity` (newest 2026-05-15). Not written by a running install. |

## Sources

| # | Source | Kind | Status | Gives |
|---|---|---|---|---|
| 1 | **Conversation databases** `<data folder>/conversations/<id>.db` (SQLite) | Local file, **undocumented** | ✅ format verified (M6 research) | **Per-call token usage**, model, step times, credits |
| — | Older conversation files `<data folder>/conversations/<id>.pb` | Local file | ❌ **encrypted** | Nothing readable |
| 2 | Local language server RPC `exa.language_server_pb.LanguageServerService/*` | **Undocumented local API** | ⏳ not verified; the live probe isn't approved | Quota per model, plan, identity |
| — | Official docs ([plans](https://antigravity.google/docs/plans)) | Documentation | ✅ | Quota is per model. It refreshes every 5 hours (Pro/Ultra) or weekly, with weekly rate limits. **No programmatic interface is documented.** |

## Conversation databases (`.db`)
**One file per conversation (trajectory).** Verified on IDE 2.5.5:

| Table | Rows (sample) | Columns |
|---|---|---|
| `trajectory_meta` | 1 | `trajectory_id` TEXT pk, `cascade_id` TEXT, `trajectory_type` INT, `source` INT |
| `steps` | 104 | `idx` pk, `step_type`, `status`, `has_subtrajectory`, **`metadata` BLOB**, `error_details`, `permissions`, `task_details`, `render_info`, `step_payload` BLOB, `step_format` |
| `gen_metadata` | 37 | `idx` pk, **`data` BLOB**, `size` |
| `executor_metadata` | 7 | `idx` pk, `data` BLOB |
| `trajectory_metadata_blob` | 1 | `id` pk, `data` BLOB |
| `battle_mode_infos`, `parent_references` | 0 | `idx` pk, `data` BLOB |

**The blobs are plain protobuf, not encrypted:**
- `steps.metadata`: 104/104 blobs parse fully as protobuf, entropy about 5.9 bits/byte.
- `gen_metadata.data`: 37/37 parse fully, entropy about 6.9 bits/byte.

The older `.pb` files are another story: entropy **7.999** bits/byte, no compression header, and they don't start like a protobuf message. They're encrypted.

**Message definitions,** taken from the protobuf descriptors compiled into `language_server_windows_x64.exe` (2.5.5: 216 `.proto` files, 15,732 messages). Each matches the field numbers and wire types found in the data:

- **`exa.codeium_common_pb.ModelUsageStats`:**
  - `1 model` (enum `Model`)
  - `2 input_tokens`, `3 output_tokens`
  - `4 cache_write_tokens`, `5 cache_read_tokens`
  - `6 api_provider` (enum)
  - `7 message_id`, `8 response_header` (map)
  - `9 thinking_output_tokens`, `10 response_output_tokens`
  - `11 response_id`, `12 provider_assigned_message_id`
  - Zero-valued fields are omitted on the wire, so 4 and 9 were absent in the sample.
- **`steps.metadata` = `exa.cortex_pb.CortexStepMetadata`:**
  - `1 created_at` (Timestamp)
  - **`9 model_usage` (`ModelUsageStats`)**
  - `11 generator_model` (enum)
  - `14 flow_credits_used`, `15 prompt_credits_used`
  - `23 tool_call_output_tokens`
  - `28 retry_infos`
  - Seen in 39 of 104 steps.
- **`gen_metadata.data` field 1 = `exa.cortex_pb.ChatModelMetadata`:**
  - `1 system_prompt` and `2 message_prompts`: **content**
  - `3 model`
  - **`4 usage` (`ModelUsageStats`)**
  - `5 model_cost`, `13 credit_cost`
  - `17 retry_infos` (each with its own `usage`)
  - `19 response_model`, `21 model_display_name`
  - Seen in 36 of 37 rows.

**Privacy.** The same records hold content: `system_prompt` (one was 30 KB), message prompts, tool calls, and `steps.step_payload` (up to 132 KB). A parser must decode **only** the usage, model and timestamp fields by number and skip everything else. Strings are never decoded, except the response id used for dedup, which is hashed.

**Not yet verified** (to settle at the start of M6):
- **Double recording:** whether a model call appears in both `steps.metadata.model_usage` and `gen_metadata` usage (counting both would double-count).
- **Model names:** where the model name comes from. The `Model` enum number needs the enum's value names, or the `response_model` / `model_display_name` strings.
- **The app install:** whether its single `.db` (app 2.0.11) has the same schema.

## Language server RPC (M2, static only)
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
- **Embedded field names:** `quota_info`, `remaining_fraction`, `reset_time`, `model_or_alias`, `user_status`, `plan_info`, `plan_name`, `user_tier`, `available_prompt_credits`, `available_flow_credits`, `monthly_prompt_credits`, `max_tokens`.
- **Discovery changed.** The daemon files (`<data folder>/daemon/ls_*.json`) are stale: dated 2026-02-26, reporting LS 1.19.4, with dead PIDs. Version 2.5.5 appears to pass its CSRF token on the process command line, so using the API would mean reading another process's arguments to obtain a secret. **Not approved;** it would be a separate decision.
- `remaining_fraction` is "remaining", not "used". A missing fraction must be treated as unknown, not full.

## Identity and entitlement
- **Identity:** no local, non-credential source has been verified. **Install-scoped**, one account per data folder (IDE vs app). The user can link them manually (M7).
- **Scope:** `google:antigravity`, **separate from Gemini CLI** even with the same Google account. Their quotas are documented separately.

## What M6 can claim

| Metric | Source | Kind | Stability |
|---|---|---|---|
| Tokens by model and conversation | conversation `.db` → `ModelUsageStats` | Local | Undocumented |
| Credits used per step | `CortexStepMetadata` 14/15 | Local | Undocumented (not a quota; shown later if useful) |
| Context used | latest step's `input_tokens + cache_read_tokens` | Local | Undocumented |
| Context total | model table | **Estimated** | — |
| Quota | — | **Unavailable**: only via the undocumented live API (not approved) | — |
| History in encrypted `.pb` conversations | — | **Unavailable** | — |

## Risks
- **Fully undocumented, internal formats.** The schema (`step_format`) and protobuf definitions can change with any Antigravity update. Verify per version, and report `UnsupportedVersion` when the tables or fields don't match.
- **The databases are live SQLite files.** Reading them in place could create `-shm`/journal files. They must be copied (with `-wal`) into AgentPulse's own folder and only the copy opened.
- **Content sits next to usage in the same records,** so the strict field-number parser and privacy tests are essential.
- Conversations stored only as encrypted `.pb` files have no recoverable usage.
