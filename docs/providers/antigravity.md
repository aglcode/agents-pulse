# Antigravity: verified data sources

**Verified:**
- **2026-11-04 (M2), statically:** the installed language server's RPC methods and protobuf field names, plus daemon discovery files (`pid`, ports and `lsVersion` only).
- **2026-11-07 (M6 research), local files only:** the conversation stores' formats and the protobuf definitions they use. Approved step by step.
- **2026-11-08 (M6 research), local files only:** double recording, model names and the app's schema, across **all 21 `.db` files** (20 IDE + 1 app). See [Settled 2026-11-08](#settled-2026-11-08). Approved.
- **2026-11-08 (M6 collector):** token-field semantics, and the collector run against the real data folders. See [M6 collector](#m6-collector-2026-11-08).

**How the 2026-11-07 checks were done:**
- **Entirely local:** no network, no calls to Antigravity's language server.
- **Antigravity's files were never opened for writing:** one `.db` was copied to a scratch folder and only the copy was opened. Each original was read through a handle that told Windows not to update its last-access time.
- **Nothing changed:** before/after snapshots of size, created, modified and last-access times were identical for `~/.gemini/antigravity-ide` (4,202 entries) and the install folder (47,916 entries). Scratch copies were deleted.
- **Printed:** table and column names, declared types, row counts, protobuf field numbers and wire types, string lengths, entropy, and the protobuf definitions compiled into the language server.
- **Never printed or kept:** any value, text, prompt or ID. `csrfToken` has never been read.

**How the 2026-11-08 checks were done:** same method as 2026-11-07.
- Antigravity wasn't running.
- Every `.db` and its `-wal` were copied to a scratch folder, and only the copies were opened.
- **Nothing changed:** snapshots of `~/.gemini/antigravity-ide` (4,202 entries), `~/.gemini/antigravity` (2,925) and the IDE install folder (47,916) were identical before and after.
  - The first attempt crashed in the descriptor parser before it took its "after" snapshot. That run is covered separately: none of the 30 files read (21 `.db`, 8 `-wal`, the language server) shows a last-access or modified time from that session.
  - Scratch copies were deleted.
- **Printed:** counts, field numbers, `Model` enum value names, and two model-name strings: `response_model` and `model_display_name`. These are the only strings decoded.
- **Response and message IDs** were hashed with a random per-run salt for matching, and never printed.

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

**Message definitions,** taken from the protobuf descriptors compiled into `language_server_windows_x64.exe` (2.5.5: 216 `.proto` files, 15,732 messages on 2026-11-07. The 2026-11-08 re-read counted 270 files, 4,457 unique message names (nested included) and 769 enums. The 11-07 counting method wasn't recorded, so the totals aren't comparable. The definitions below were re-read on 11-08 and match exactly). Each matches the field numbers and wire types found in the data:

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
- **`gen_metadata.data` = `exa.cortex_pb.CortexStepGeneratorMetadata`:**
  - `1 chat_model` (`ChatModelMetadata`, below)
  - `2 step_indices` (the `steps.idx` it produced)
  - `3 planner_config`, `4 execution_id`, `5 error`, `7 injected`, `8 mendel_experiment_ids`
  - `9 prompt_debug_str`: **content**
  - `10 has_ever_used_non_google_model`
- **`exa.cortex_pb.ChatModelMetadata`:**
  - `1 system_prompt` and `2 message_prompts`: **content**
  - `3 model`
  - **`4 usage` (`ModelUsageStats`)**
  - `5 model_cost`, `13 credit_cost`
  - `17 retry_infos`
  - `19 response_model`, `21 model_display_name`, `22 response_model_full`
  - Other fields (tools, prompt sections, durations, config) aren't used.
  - Seen in 36 of 37 rows.
- **`exa.cortex_pb.RetryInfo`** (used by both tables):
  - `1 sherlog_link`, `2 usage` (`ModelUsageStats`), `3 error`, `4 trace_id`, `5 retry_reason`, `6 consumed_credits`

**Privacy.** The same records hold content: `system_prompt` (one was 30 KB), message prompts, `prompt_debug_str`, tool calls, and `steps.step_payload` (up to 132 KB). A parser must decode **only** the usage, model, timestamp and model-name fields by number and skip everything else.
- The only strings decoded are `response_model` and `model_display_name`. They're accepted only if they look like a short model name.
- The response id is used for dedup and is hashed.

## Settled 2026-11-08
Checked across all 20 IDE databases, plus the app's one in check 3.

### 1. Double recording: yes. Count from `steps` only.
- **Every generation record is a duplicate of a step.**
  - Steps: 714 usage records. Generation: 694.
  - All 676 generation records that have a response id match a step's response id, and 672 of them have identical token counts.
  - For the 4 that differ slightly, the step's value is used.
- **No response id repeats** within `steps`.
- **Steps have 38 calls that `gen_metadata` doesn't:**
  - 32 are `step_type` 23 calls with model `1050`, which never appear in `gen_metadata`.
  - 6 are ordinary model steps (`step_type` 15).
- **Small residual:** 18 generation records have usage but no response id or model, and match no step by token counts. Leaving them out is the conservative choice (never inflate). Their size relative to the totals wasn't measured.
- **Retries aren't extra calls.** All 714 `retry_infos` usage records carry the response id of a main usage record, so they describe the final attempt.
- **Timestamps:** every usage step has `created_at`, while `gen_metadata` has no timestamp. Time buckets come from steps.

### 2. Model names: the enum is useless, the names come from `gen_metadata`.
- The 2.5.5 `Model` enum has 691 values, and **651 are named `MODEL_PLACEHOLDER_Mnn`**. All 6 numbers seen in the data are placeholders.
- `ChatModelMetadata.19 response_model` is set on every generation record that has a model, with one value per enum number. `21 model_display_name` is set on most records, but not all.
- The display name includes the mode, so 1298 and 1299 are the same model at two settings.

  | Enum # | `response_model` | `model_display_name` | Calls (IDE) |
  |---|---|---|---|
  | 1016 | `gemini-pro-default` | Gemini 3.1 Pro (High) | 365 |
  | 1026 | `claude-opus-4-6-thinking` | Claude Opus 4.6 (Thinking) | 253 |
  | 1035 | `claude-sonnet-4-6` | Claude Sonnet 4.6 (Thinking) | 20 |
  | 1298 | `gemini-3.7-flash` | Gemini 3.7 Flash (High) | 41 |
  | 1299 | `gemini-3.7-flash` | Gemini 3.7 Flash (Medium) | 3 |
  | 1050 | — (no generation record) | — | 32 |

- **Approach:** each scan builds the number-to-name map from the user's own `gen_metadata`. No table is shipped with AgentPulse. The fields read are `1.3`, `1.4.1`, `1.19` and `1.21`.
  - *Superseded by the collector:* `response_model` is preferred, then the display name, and a number with neither stays unnamed. See [M6 collector](#m6-collector-2026-11-08).

### 3. App 2.0.11: same schema.
- The app's database has the same 7 tables, with column names and types identical to the IDE's.
- `step_format` is 0 everywhere: IDE 2,247 steps, app 137.
- All blobs parse.
- 46 step usages and 44 generation usages, and all 44 generation records match a step by response id.
- **Models seen:** 1016, 1026 and 1050. The `response_model` strings match the IDE's mapping. The app's records have no display names.
- The 2.0.11 enum wasn't read; it isn't needed, since names come from the data.

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
| Tokens by model and conversation | conversation `.db` → `steps.metadata.model_usage`, one per response id (`gen_metadata` usage is a duplicate and isn't counted) | Local | Undocumented |
| Model name | `gen_metadata` → `response_model` (else `model_display_name`), keyed by enum number within the same conversation; otherwise unnamed (`unknown`) | Local | Undocumented |
| Credits used per step | `CortexStepMetadata` 14/15 | Local | Undocumented (not a quota; shown later if useful) |
| Context used | latest model step's `input_tokens + cache_read_tokens + cache_write_tokens` | Local | Undocumented |
| Context total | — | **Unavailable**: Antigravity doesn't record its window, and its limits may differ from the model's own (changed from "Estimated" in the collector) | — |
| Quota | — | **Unavailable**: only via the undocumented live API (not approved) | — |
| History in encrypted `.pb` conversations | — | **Unavailable** | — |

## Risks
- **Fully undocumented, internal formats.** The schema (`step_format`) and protobuf definitions can change with any Antigravity update. Verify per version, and report `UnsupportedVersion` when the tables or fields don't match.
- **The databases are live SQLite files.** Opening them with SQLite in place could create `-shm`/journal files, and a copy on disk would hold conversation content. The collector reads the `.db` and `-wal` into memory and opens them there (see below).
- **Content sits next to usage in the same records,** so the strict field-number parser and privacy tests are essential.
- Conversations stored only as encrypted `.pb` files have no recoverable usage.
- **`Model` enum numbers are placeholders** and could be reassigned between versions. Building the name map from each database's own records keeps a renumbering from mislabelling older conversations.

## M6 collector (2026-11-08)
`crates/collectors/src/antigravity/`, collector id `antigravity`.

**Files opened** (read-only, into memory only):
- `~/.gemini/antigravity-ide/conversations/<id>.db` and `<id>.db-wal` (IDE)
- `~/.gemini/antigravity/conversations/<id>.db` and `<id>.db-wal` (app)

**Never opened:**
- `*.pb`, `*.db-shm`, `brain/`, `knowledge/`, `daemon/` (CSRF token)
- `~/.gemini/antigravity-backup/`
- The language server is never called, and no process is inspected.

**How a database is read:**
1. The `.db` and its `-wal` are read into memory. The pair is re-read if either changes in the meantime (up to 3 tries, otherwise the next run tries again).
2. The log's committed frames are replayed in memory, with salts and checksums checked as in SQLite's file format.
3. The image is opened as SQLite's in-memory database (rusqlite `serialize`). Nothing is created in Antigravity's folders, and no copy of a conversation is ever written to disk.
4. A database whose metadata (size and modified time of `.db` + `-wal`) hasn't changed since the last run isn't read again.

**Token semantics** (checked 2026-11-08 on all 760 usage records, same read-only method; counts only):
- **Output:** `output_tokens` = `thinking_output_tokens` + `response_output_tokens` in every record, so output already includes thinking. Mapping: `output` = `output_tokens`, `reasoning` = `thinking_output_tokens`.
- **Input:** cache reads exceed `input_tokens` in most records (e.g. 319 of 402 for model 1016), so `input_tokens` is fresh input only. Mapping: `input` = `input_tokens`, `cache_read` = `cache_read_tokens`, `cache_write` = `cache_write_tokens` (always 0 so far).
- **Field encoding:** zero-valued fields are omitted on the wire. `cache_write` and `reasoning` are therefore recorded only when nonzero; Claude models report no thinking count.
- **Duplicates across files:** every response id appears in exactly one database. The dedup key is the install plus the hashed response id (or conversation + step index when a step has no id).
- **Status:** every usage step has `status` 3 and `has_subtrajectory` 0. Only `step_type` 15 (the conversation's own calls) and 23 (auxiliary calls, model 1050) carry usage.

**Mapping:**
- **Session:** one per conversation database, with its first and last step times.
- **Context used:** the latest call that has a `generator_model`. Auxiliary calls count as usage but don't set the context.
- **Model names:** taken from that conversation's own generation records. A model with no name there is stored as `unknown`; model 1050 is always unnamed.
- **Unsupported data:**
  - A row in an unknown `step_format`, or with unexpected wire types on the fields used, is skipped and counted.
  - A database without the verified tables reports tokens as `UnsupportedVersion` when no other database could be read in that run.

**Verified on real data** (2026-11-08, Antigravity not running):

| Install | Databases | Sessions | Usage records | Records per model |
|---|---|---|---|---|
| IDE 2.5.5 | 20 | 20 | **714** | gemini-pro-default 365 · claude-opus-4-6-thinking 253 · gemini-3.7-flash 44 · claude-sonnet-4-6 20 · unnamed 32 |
| App 2.0.11 | 1 | 1 | **46** | gemini-pro-default 37 · claude-opus-4-6-thinking 7 · unnamed 2 |

- These match the research counts above (714 step records; 44 = 41 + 3 for the two Gemini 3.7 Flash modes).
- A full read of both installs takes about 0.2–0.35 s.
- **Before/after snapshots** of both data folders and the IDE install folder (55,043 entries) showed no file created, removed or modified.
  - The first run updated the last-access time of the 28 files it read (21 `.db`, 7 `-wal`), as any read does with Windows last-access updates on.
  - The second run changed nothing.
- The values haven't been compared with Antigravity's own UI yet: it shows quota, not token counts.
