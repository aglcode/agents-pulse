# Claude Code: verified data sources

**Verified:** 2026-11-04 against Claude Code **2.1.318** on Windows 11. Account: `authMethod: claude.ai`, `subscriptionType: pro`.

**How this was checked:** key names and value types only. No prompt, response, tool output or file content was read. `~/.claude/.credentials.json` was not opened.

## Sources, in blueprint priority order

| # | Source | Kind | Status | Gives |
|---|---|---|---|---|
| 1 | `claude auth status --json` | Official CLI | ✅ verified locally | Identity, auth method, plan |
| 2 | **Status line stdin JSON** ([docs](https://code.claude.com/docs/en/statusline)) | Documented local interface | ✅ documented; ⏳ not yet exercised live | **Subscription quota** (5h / 7d), **context window**, model, session id |
| 3 | OpenTelemetry ([docs](https://code.claude.com/docs/en/monitoring-usage)) | Official telemetry | ✅ documented | Per-request tokens, cost estimate, model, `user.account_uuid` (no quota) |
| 4 | `~/.claude/projects/**/*.jsonl` | Local file, **undocumented format** | ✅ verified locally (256 files) | Per-message tokens and model, back-filled history |

Not used: `~/.claude.json` (`oauthAccount.*`). It's an internal file, and source 1 gives identity officially.

## Identity and entitlement
> **Changed in M4 (2026-11-06):** `claude auth status --json` is **not used**. Running it writes to the config folder: with `CLAUDE_CONFIG_DIR=~/.claude` it created `~/.claude/.claude.json` and rotated a file in `~/.claude/backups/`. AgentPulse never changes a tool's files, so identity comes from reading `.claude.json → oauthAccount` instead (see Implementation below). The notes that follow are the M2 findings.

- **Identity:** `claude auth status --json` → `email`, `orgId`, `authMethod`, `subscriptionType`, `loggedIn`. This gives **VerifiedEmail + org**. It doesn't return the account UUID; OTel does (`user.account_uuid`), so the identity can be upgraded to VerifiedId when telemetry is on.
- **Scope:**
  - `authMethod: claude.ai` → `anthropic:claude_subscription`. Evidence: the status line `rate_limits` are documented as the claude.ai Pro/Max/Ultra plan windows, so Claude Code consumes the plan.
  - API-key or gateway auth → `anthropic:api`.
- **Organization:** `orgId` is part of the account key (seat quotas are per org), as the architecture planned.

## Usage (tokens)

**From session JSONL** (`type: "assistant"` lines):

| Field | Path |
|---|---|
| Dedup id | `message.id` (also `requestId`) |
| Model | `message.model` |
| Input / output | `message.usage.input_tokens`, `message.usage.output_tokens` |
| Cache read / write | `message.usage.cache_read_input_tokens`, `message.usage.cache_creation_input_tokens` (+ `cache_creation.ephemeral_5m/1h_input_tokens`) |
| Reasoning | `message.usage.output_tokens_details.thinking_tokens` (often null) |
| Session / time | `sessionId`, `timestamp` (ISO string) |
| Subagent | `isSidechain`, `agentId`; transcripts under `…/subagents/` (86 of 256 files) |

**Dedup behaviour (measured on 24,598 lines in 60 recent files):**
- **Repeats are normal.** 1,763 of 2,430 messages appear on more than one line (up to 22; roughly one line per content block).
- **Only `output_tokens` ever differs between repeats** (23 messages), and it **never decreases** (1,763 of 1,763).
- **No message id appears in two files** (10,350 ids across 256 files), so resumed sessions don't copy history.
- **Dedup key:** `claude:{message.id}`.
- **`source_seq`:** `(file_id << 40) | byte_offset`. The last line wins and replaces the full token vector. This matches the architecture §5.4 design.

**From OTel (optional upgrade):** `claude_code.api_request` events with `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `model`, `request_id`, `cost_usd` (an estimate). Dedup key `claude:req:{request_id}`. Enabling it requires setting environment variables for Claude Code and running an OTLP receiver, which is a larger change.

## Subscription quota
- **Source:** status line JSON → `rate_limits.five_hour.{used_percentage, resets_at}` and `rate_limits.seven_day.{used_percentage, resets_at}` (epoch seconds), plus `rate_limits.spend_limit.*` behind a gateway.
- **Availability limits:**
  - It's present only for Pro/Max/Ultra (or gateway spend limits), and only after the session's first API response.
  - Each window can be absent independently, and is dropped once its `resets_at` passes.
- **Classification:** SourceKind `Local`, Stability `Documented`. Windows map to `window_kind` `5h` / `7d`.
- **Catch: this needs an opt-in config change.** The status line runs only if the user configures a `statusLine` command in `~/.claude/settings.json`. **None is configured on this machine.**
  - AgentPulse would install a tiny "tap" command that:
    - writes only the numeric quota and context fields, plus a hashed session id, to an AgentPulse-owned file;
    - then runs the user's previous status line command (if any) unchanged.
  - It must be explicit, shown in full, and reversible.
- **Refresh:** when a session starts, on every assistant message, on `/compact`, and when a window's `resets_at` passes. There are no updates while Claude Code isn't running, so freshness rules apply.

## Context window
- **Status line (preferred):** `context_window.used_percentage`, `context_window_size`, `total_input_tokens` (documented). Stability `Documented`.
- **JSONL fallback:** used = `input_tokens + cache_creation_input_tokens + cache_read_input_tokens` of the session's latest assistant message. This is the same formula the docs give for `used_percentage`. The total isn't in JSONL (200k or 1M depending on model and beta), so `total_tokens` is **Estimated** from a model table with a stated basis.

## Privacy
- **Never read:** `~/.claude/.credentials.json`.
- **Content-bearing fields to skip:** `message.content`, `toolUseResult`, and the `user`, `attachment`, `api-request*`, `file-history-*` and `last-prompt` line types. **`file-history-snapshot` uses file paths as JSON keys**, so a generic key walk leaks paths and the parser must not walk it.
- **Status line tap:** `transcript_path`, `cwd` and `workspace` arrive on stdin and **must be dropped**, not written.
- **OTel:** prompts, responses and tool details are redacted by default (`OTEL_LOG_USER_PROMPTS`, `OTEL_LOG_ASSISTANT_RESPONSES` and `OTEL_LOG_TOOL_DETAILS` default off). AgentPulse must never enable them.

## What the collector would claim

| Metric | Source | Kind | Stability |
|---|---|---|---|
| Tokens by model and session | JSONL | Local | Undocumented |
| Quota 5h / 7d | Status line tap (opt-in) | Local | Documented |
| Context used / total | Status line tap (opt-in) | Local | Documented |
| Context (no tap) | JSONL + model table | used: Local / total: **Estimated** | Undocumented |
| Identity | `claude auth status --json` | Official CLI | Documented |
| Quota without the tap | — | **Unavailable** (`OptInRequired`) | — |

## Risks
- The JSONL format is undocumented and changed shape often (24 line types seen). The parser must be narrow, version-tolerant and schema-validated.
- The tap changes a user config file. It must chain the existing status line, and must not break it if AgentPulse is uninstalled (the command must fail safe and print nothing).
- `claude auth status` spawns Node, so call it on detect only (minutes apart), never per batch.

## Implementation (M4, 2026-11-06)
Code: `src-tauri/crates/collectors/src/claude_code/`.

**Read-only.** AgentPulse never writes to, runs or reconfigures Claude Code: no status line tap, no `claude` subprocess, no settings edits. A test (`collecting_never_writes_to_the_config_folders`) checks that every file's size, mtime and contents are unchanged after collecting.

- **Config folders:** each folder is its own installation (`install_key` = hash of the normalized folder path), with its own identity, account and batch.
  - Read automatically: `~/.claude` (default), plus `CLAUDE_CONFIG_DIR` if AgentPulse's own process can see it. On this machine it can't: the variable isn't set at the Windows user or system level, only in some terminals.
  - Added by the user on the Claude Code card. A `.claude-*` folder in the home that has a `projects` folder is **suggested**, never added automatically.
  - Added folders are saved in `app_settings` under `claude_code.added_folders`. Changing them restarts the collector's worker (`Engine::restart`) so file watching follows.
- **Identity:** `.claude.json → oauthAccount` (undocumented). Only `accountUuid`, `emailAddress`, `organizationUuid` and `organizationType` are deserialized; the rest of the file (project history, settings) is skipped unread.
  - The file is `~/.claude.json` for the default folder (falling back to `~/.claude/.claude.json`), and `<folder>/.claude.json` for any other folder.
  - `accountUuid` → **VerifiedId** (kept as a secret, only hashed). `organizationUuid` is part of the account key. Label: `email (Plan)`, where the plan comes from `organizationType` (`claude_team` → Team).
  - Scope: an `oauthAccount` means a claude.ai login → `anthropic:claude_subscription`. No `oauthAccount` → `anthropic:api`, install-scoped, labelled "API key or signed out".
  - The file is re-read only when its mtime or size changes.
- **Tokens:**
  - From `projects/**/*.jsonl` (subagent transcripts included). A byte prefilter skips every line without `"assistant"`, so user, tool-result, attachment and `file-history-snapshot` lines are never deserialized.
  - Dedup `claude:{message.id}`. `source_seq` = the line's byte offset, so the later (fuller) line of a repeated message wins. Every message lives in exactly one file (M2), so offsets never compete across files.
  - `<synthetic>` messages (local errors, no API call) are skipped.
- **Incremental reads:** every assistant line is self-contained (session id, model, timestamp), so a changed file is read **from its cursor only**. A file whose first bytes changed, or that shrank, is read again from the start; dedup makes the replay a no-op. A partial last line is left for the next run.
- **Context:** per session, the latest **main-thread** request: `input + cache_creation + cache_read` (Local). The total is **Estimated**: 200k, or 1M once usage passes 200k. Subagent requests never drive a session's context.
- **Quota:** **Unavailable**, with the new reason `requires_tool_settings_change`. It's only exposed through a status line command, which would mean editing Claude Code's settings.
- **Triggers:** a recursive watcher on each folder's `projects` (2 s debounce), plus a 5-minute fallback poll. The run timeout is 300 s, for the first backfill.

### Real-data check (this machine, read-only)
Release build, both folders, first full backfill: **2.3 s** for about 593 MB of logs.

| Folder | Files | Sessions | Messages | Input | Output | Cache read | Cache write | Identity |
|---|---|---|---|---|---|---|---|---|
| `~/.claude` | 251 | 155 | 9,966 | 317,538 | 7,904,466 | 2,156,657,267 | 41,208,723 | VerifiedId (Team) |
| `~/.claude-personal` | 14 | 11 | 1,456 | 3,150 | 2,213,288 | 462,620,135 | 6,738,390 | VerifiedId (Pro) |

**Cross-check:** an independent Python count (last line per `message.id`, `<synthetic>` excluded), sharing no code with the collector, gave **identical** numbers for `~/.claude`. For `~/.claude-personal` it found 1 more message (1,457), because an active Claude Code session was writing to that folder between the two runs.

Not yet compared against Claude Code's own `/usage` or `/cost` screens: those need Claude Code to run, which writes to the folder.

## Quota tap (M4b, 2026-11-06)
Approved as "option 3": an opt-in **Enable quota** button per config folder. It's the only write AgentPulse ever makes to a Claude Code folder.

- **Enabling:** after the user sees and confirms the exact lines, AgentPulse adds one block to `<folder>/settings.json`:
  ```json
  "statusLine": {
    "type": "command",
    "command": "~/…/agents-pulse.exe --statusline-tap <folder key>"
  }
  ```
  - **Text edit:** the block is inserted as text in Claude Code's own `JSON.stringify(…, null, 2)` format. Every other byte (key order, spacing, CRLF) is unchanged, and each edit is verified by parsing: the result must equal the original plus exactly `statusLine`.
  - **Refuses** to replace a status line the user already has, and refuses to apply if the file changed after the preview (e.g. a `/model` save in between).
  - **Writes** go to a temp file, then replace `settings.json` in one rename.
  - **Disabling** removes exactly that block. It also works after Claude Code re-saves the file (changed model or effort, new keys added) or after a sorted-key re-save. If the block was edited by hand, AgentPulse refuses rather than guess.
- **Command:** on Windows, Claude Code runs status lines through Git Bash, or PowerShell when Git Bash is absent ([docs](https://code.claude.com/docs/en/statusline#windows-configuration)). So the path uses forward slashes and `~` for the home folder, which is documented to work in both. A path that still contains a space is refused (`unsupported`).
- **The tap** is AgentPulse's own executable in tap mode. `main()` handles `--statusline-tap` before Tauri starts, so no window opens and the single-instance check doesn't run.
  - It reads the documented stdin JSON and keeps only `rate_limits.five_hour` / `seven_day` (`used_percentage`, `resets_at` in epoch seconds → ms), `context_window.total_input_tokens` / `context_window_size`, and a hash of `session_id`. `cwd`, `transcript_path`, `workspace`, `model`, cost and the rest are never named.
  - It overwrites one file, `%APPDATA%\com.angelo-almonte.agents-pulse\statusline\<folder key>.json`, using a temp file and rename.
  - It prints `5h 24% · 7d 41% · ctx 8%`. On any error it prints nothing and exits 0.
- **Collector:** reads `settings.json` (read-only) to see whether the tap is on, and watches the snapshot folder.
  - A new snapshot becomes quota readings `5h` / `7d` (Local, **Documented**) and that session's context with the **real** window size, replacing the log estimate.
  - Tap off → `requires_tool_settings_change`. Tap on with no rate limits yet → `not_yet_collected`: rate limits arrive only after a session's first reply, and only for Pro/Max/Team.
  - Readings go stale while Claude Code isn't running.
- **Tests (temp folders only; no real Claude folder was touched):**
  - an enable → disable round trip on the user's own settings text is byte-identical (LF and CRLF);
  - survives a simulated `/model` re-save;
  - never replaces the user's own status line;
  - stale preview refused;
  - tap privacy sentinel (paths, names, cost, session id in clear);
  - bad input or key → silent;
  - collector quota and context end to end;
  - tap off → old snapshot ignored.
- **Shell check (scratch data folder, no Claude folder involved):** the exact `~/…/agents-pulse.exe --statusline-tap <key>` command, with the documented JSON on stdin, printed `5h 24% · 7d 41% · ctx 8%` and wrote one snapshot with no path text, through both **Git Bash** (about 170 ms warm, 1.6 s cold first start; debug build) and **Windows PowerShell 5.1** (about 0.6 s, mostly PowerShell startup).
- **Not verified live:** clicking Enable on the user's real folders, and Claude Code actually running the command. The user is using both folders, so that first run is theirs to do.
- **Supported plans (decided 2026-11-06): Pro, Max and Ultra only.** A folder is read only when its `oauthAccount.organizationType` is `claude_pro`, `claude_max` or `claude_ultra`. These are also the plans the status line docs say report `rate_limits`.
  - Team, Enterprise, API-key and signed-out folders are listed with "Not read: <plan>" and are never read: no logs, no watcher, no batches.
  - **Remove from AgentPulse** on such a folder (preview first) does three things:
    - deletes AgentPulse's stored data for the accounts bound only to that folder (`AdminOp::ForgetInstall`; an account also bound to another folder is kept);
    - takes the quota tap out of its `settings.json` if it's on;
    - stops listing it, if the user had added it.
  - Nothing is deleted automatically: a transient read failure must never cost history.
