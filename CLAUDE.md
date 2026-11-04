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
