# AgentPulse

Local-first desktop app (Tauri 2 + Rust + React/TS + SQLite) that consolidates AI coding-assistant usage, subscription quota, context consumption and sessions for individual users on Windows and macOS.

- **Source of truth:** `AGENTPULSE_BLUEPRINT.md` (goals, stack, roadmap).
- **Architecture:** `docs/ARCHITECTURE.md` (approved design: crates, schema, collectors, runtime, privacy).
- **Provider facts:** `docs/providers/<provider>.md` (verified data sources; written in M2).

## Commands
- `pnpm install`: install frontend deps (pnpm only, no npm/yarn lockfiles)
- `pnpm tauri dev`: run the desktop app
- `pnpm build`: type-check + build the frontend
- `cd src-tauri && cargo test --workspace`: Rust tests (also regenerates TS bindings in `src/ipc/bindings/`)
- `cd src-tauri && cargo clippy --workspace --all-targets`: lint (should be warning-free)
- `scripts/perf/measure.ps1`: idle memory/CPU of a running build; record results in `docs/perf.md`
- On this machine, release builds may need `-j 4`: unbounded parallel rustc jobs crashed with `STATUS_STACK_BUFFER_OVERRUN`

## Layout
- `src-tauri/crates/core`: domain types, `Metric<T>`, `Collector` trait, registry (entitlement scopes)
- `src-tauri/crates/store`: SQLite (single writer thread, pooled readers, migrations in `migrations/`)
- `src-tauri/crates/engine`: supervisor, identity resolver, event bus
- `src-tauri/crates/collectors`: one module per provider (`mock` is dev-only)
- `src-tauri/src`: thin Tauri glue (commands, event bridge, tray, shutdown)
- `src/ipc`: the only place the frontend calls `invoke`/`listen`

## Workflow
- Work milestone by milestone (architecture §11). Implement, test, then **stop for approval** before the next milestone.
- Don't add technologies or dependencies beyond the architecture doc without justification.

## Non-negotiable rules
- **Never fabricate metrics.** Every metric carries a source: Official API / Official Telemetry / Local / Estimated / Unavailable. Unavailable shows a reason, never `0`. Estimated and stale values are always labelled.
- **Subscription quota ≠ token usage ≠ session context.** They are separate types and are never converted into one another.
- **Identity ≠ usage account.** A shared login doesn't mean shared quota. Merge only when the provider documents a shared entitlement, or when the user links accounts manually.
- **No prompts, source code, conversation content or tool output** are collected, persisted or logged. Parsers deserialize usage fields only, and every parser has a privacy test.
- **Never read provider credential files** (`~/.claude/.credentials.json`, `~/.codex/auth.json`, `~/.gemini/oauth_creds.json`). Secrets live in `secrecy` types in memory, and owned credentials go in the OS keychain, never in SQLite or logs.
- Don't assume undocumented APIs exist. Verify against the installed version and label undocumented sources.
- Performance targets are benchmarks to measure (`docs/perf.md`), not assumptions. No sub-60s polling.

## Read-Only External Agent Environment (STRICT)

This is a non-negotiable security requirement for AgentPulse and every AI coding agent working on this repository.

**AgentPulse and all coding agents MUST treat external AI agent directories and their contents as READ-ONLY.**

The user's existing AI coding environments must NEVER be modified, disrupted, or managed by AgentPulse or any coding agent.

### 1. Protected Directories

Treat the following directories and their contents as strictly read-only:

- Claude Code: `~/.claude/`
- Codex: `~/.codex/`
- Gemini CLI: `~/.gemini/`
- Google Antigravity: `~/.gemini/antigravity/`
- Cursor: its user data, settings, extensions, and configuration directories.
- Windsurf: its user data, settings, extensions, and configuration directories.
- Any other installed AI coding agent's local directories.

On Windows, this includes their equivalent directories under `%USERPROFILE%` and `%APPDATA%`.

The paths above are examples, not an exhaustive list. Discovering another agent's directory automatically makes it protected.

### 2. Absolutely Forbidden Operations

Agents MUST NOT:

- Create files or folders inside protected directories.
- Modify, overwrite, append to, or truncate existing files.
- Delete, rename, move, or relocate files or folders.
- Install, uninstall, update, or reconfigure AI coding agents.
- Modify agent settings, preferences, instructions, rules, or configuration files.
- Modify authentication, session, credential, or account data.
- Clear, clean, or delete agent caches, histories, logs, sessions, or databases.
- Modify environment variables or system configuration belonging to other agents.
- Create symlinks, junctions, scheduled tasks, or background services that modify or interfere with protected environments.

**No exceptions for cleanup, optimization, debugging, testing, or convenience.** The only exception is the Claude Code quota tap in §7.

### 3. Read-Only Collector Policy

AgentPulse collectors must operate in read-only mode when accessing external agent environments.

- Read only explicitly approved metadata and usage sources.
- Never write back to provider directories.
- Never modify provider databases, JSON files, JSONL files, configuration, or session data.
- Never delete or clean provider-generated files.
- Never take ownership of provider processes or manage their lifecycle.
- Never interfere with an agent's normal operation.
- If a data source requires modifying an external agent environment, DO NOT implement that integration (the only exception is §7).

All collected information must be treated as observational data.

### 4. AgentPulse Write Boundaries

AgentPulse may write only to its own designated application directories:

- Its repository during authorized development.
- Its own application data directory.
- Its own SQLite database.
- Its own logs, cache, configuration, and generated files.
- Its own OS keychain entries, with explicit authorization where required.

AgentPulse must never use an external AI agent's directory as a storage location.

### 5. Verification Requirements

Before implementing a collector or filesystem integration:

1. Identify every filesystem path the implementation accesses.
2. Classify each path as protected external data or AgentPulse-owned storage.
3. Verify that external paths are accessed exclusively through read-only operations.
4. Add tests ensuring collectors never perform filesystem writes to protected directories.
5. Verify that errors, unsupported formats, and permission failures do not trigger fallback write operations.

### 6. Final Security Rule

**AgentPulse is an observer, not a manager, of other AI coding environments.**

It must monitor available usage and session metadata without changing the user's existing AI agents, their configurations, their files, their caches, their authentication, or their normal operation.

When uncertain whether a filesystem operation could modify an external agent environment, DO NOT perform it. Stop and request explicit user approval.

### 7. Sole Exception: Claude Code Quota Tap (approved 2026-11-06, reconfirmed 2026-11-07)

Claude Code exposes subscription quota only through a status line command, so AgentPulse may make **one** user-initiated edit to a Claude Code config folder. Every condition below must hold:

- **Scope:** only `settings.json` in a Claude Code config folder (`~/.claude/` or a `CLAUDE_CONFIG_DIR` folder such as `~/.claude-personal/`), and only its top-level `statusLine` key. No other file or key, in Claude Code or any other agent.
- **Content:** the block contains only AgentPulse's own tap command (marked `--statusline-tap`).
- **User-initiated:** it runs only when the user confirms a previewed change on that folder's card in AgentPulse:
  - **Enable quota** → **Add to settings.json**
  - **Disable quota** → **Remove from settings.json**
  - **Remove from AgentPulse**, which takes out the tap first when it's on

  It never runs automatically: not at startup, during collection, on uninstall or as a fallback.
- **Non-destructive:**
  - Enabling refuses if the user already has a status line.
  - Removing deletes only AgentPulse's own block.
  - Applying refuses if the file changed since the preview.
  - It's a text edit, so every other byte is preserved, and the result is verified by parsing.
- **Atomic write:** the only other file created is the temporary `settings.json.agentpulse-tmp`, which is renamed over `settings.json` (or deleted if the rename fails).
- **Single code path:** `crates/collectors/src/claude_code/settings_edit.rs` is the only code allowed to perform this write. Collectors stay read-only, and the collection-path "never writes" tests must keep passing.
- The tap's snapshots go to AgentPulse's own app-data folder, never the Claude Code folder.

Any other write to an external agent environment, including extending this exception, needs explicit user approval and an update to this section first.

## Build & Repository Hygiene

All coding agents working on AgentPulse MUST follow these rules whenever they build, test, compile, or modify the project.

### 1. Keep the Project Lightweight

- Prevent generated build artifacts, temporary files, obsolete bundles, and unnecessary caches from accumulating inside the repository.
- Never commit compiled binaries, Rust build outputs, frontend bundles, temporary files, or local development data.
- Keep the repository focused on source code, configuration, documentation, migrations, and required assets.
- Inspect disk usage when a build or development task generates unusually large files or directories.

### 2. Build Artifact Cleanup

**After every task that builds or tests the project, always run both cleanups below.** This is a standing user request (2026-11-07, reinforced 2026-11-11 after it was skipped and the project reached 4.08 GB). Do it once the task is finished, not between steps of the same task, and report the project size before and after.

Cleanup is mandatory and part of finishing the task:

- **Last step, every time.** If a task ran any `cargo` command (`build`, `test`, `clippy`, `tauri dev`/`build`) or `pnpm build`, the cleanup is the last action before the final message.
- **No exceptions.** This applies when the task ends in a commit, a PR or a milestone stop for approval, and when the build or tests failed.
- **Report it.** The final message must give the project size before and after. A final message without this report means the task is not finished.
- **Never deferred.** Don't skip the cleanup because the next task will rebuild anyway.

In addition:

- Remove obsolete Rust build artifacts when they are no longer needed.
- Remove disposable frontend build outputs and obsolete bundles.
- Remove temporary test results, coverage reports, profiling outputs, and abandoned generated files.
- Clean unnecessary project-local caches when they can safely be regenerated.
- Do not leave large, disposable build artifacts in the project unnecessarily.

**Rust build cleanup:**

```bash
cargo clean --manifest-path src-tauri/Cargo.toml
```

**Frontend build cleanup:**

```bash
rm -rf dist
```

Use equivalent PowerShell commands on Windows.

### 3. Cache Management

- Identify which caches are project-local and which belong to global dependency managers.
- Clean obsolete or unnecessary project-local caches when safe.
- Do not automatically delete global Cargo, pnpm, or other dependency caches.
- Within a task, keep the incremental compilation cache between builds (prefer incremental builds during active development). Clear it when the task is done, as §2 requires.
- If a build directory becomes unusually large, inspect its contents before deciding what to remove.

### 4. Protect Important Files

NEVER automatically delete:

- Rust, React, TypeScript, or other source files.
- `Cargo.toml`, `Cargo.lock`, `package.json`, or `pnpm-lock.yaml`.
- `node_modules/` or global dependency caches.
- SQLite databases, migrations, or application data.
- Environment configuration, credentials, certificates, or secrets.
- Git history, branches, or repository metadata.
- Required Tauri-generated schemas, bindings, icons, and application assets.

Never use broad deletion commands without verifying the exact target path.

### 5. Git Ignore Requirements

Ensure `.gitignore` excludes generated and local-only files, including:

```gitignore
# Rust
**/target/

# Frontend
node_modules/
dist/
build/

# Test and coverage
coverage/
.nyc_output/

# Logs and temporary files
*.log
logs/
tmp/
temp/

# Local environment files
.env
.env.*
!.env.example

# OS and IDE
.DS_Store
Thumbs.db
```

Add other project-specific generated files when necessary.

Do not ignore source-controlled migrations, required schemas, or files needed for reproducible builds. Only ignore local SQLite databases when they are genuinely runtime data rather than required fixtures or test assets.

### 6. Repository Size Verification

Before finishing a substantial task:

1. Inspect newly generated files and directories.
2. Identify unusually large files and folders.
3. Remove unnecessary generated artifacts when safe.
4. Verify that important source files and application data remain intact.
5. Run `git status` and check that generated artifacts are not staged.
6. Check whether the repository size has increased unexpectedly.

### 7. Final Build & Cleanup Report

Include a short report when completing substantial implementation tasks:

- Build and test commands executed.
- Generated artifacts created.
- Cleanup actions performed.
- Remaining unusually large directories or files.
- Whether generated files were accidentally staged.
- Confirmation that source code and application data were preserved.

**Core Rule:** AgentPulse must remain lightweight, clean, and reproducible. Never allow unnecessary build artifacts, cache bundles, or temporary files to inflate the project. Cleanup must be safe, deliberate, and verified.
