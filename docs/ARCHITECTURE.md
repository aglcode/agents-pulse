# AgentPulse — Architecture (Revision 3, approved 2026-11-03)

## Context

The repo is a fresh `create-tauri-app` scaffold (Tauri 2, React 19, TS 6, Vite 8, Rust edition 2024) with only the `greet` demo command. Tailwind v4 is in devDependencies but not wired up. Both npm and pnpm lockfiles exist, and `tauri.conf.json` still calls `npm`. `AGENTS.md` duplicates `AGENTPULSE_BLUEPRINT.md`. The untracked `docs/ARCHITECTURE.md` is a task note, and the approved version of this document will replace it.

Revision 2 was approved overall. Revision 3 only addresses the five final review points. The stack, the four-crate workspace and the milestones are unchanged. Approved on 2026-11-03; implementation proceeds milestone by milestone (§11).

**Local facts checked on this machine (key/file names only, no content read):**
| Provider | Usage data | Identity available locally |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` | `~/.claude.json → oauthAccount.{accountUuid, organizationUuid, emailAddress, …}`. Not a credential file. |
| Gemini CLI | OTel (documented), `~/.gemini/tmp/` | `~/.gemini/google_accounts.json → active` (email). Not a credential file. |
| Antigravity | Local LS: `~/.gemini/antigravity/daemon/ls_*.json → {pid, httpsPort, httpPort, lspPort, lsVersion, csrfToken}`. Two installs (`Antigravity/`, `Antigravity IDE/`). | Unknown. Must come from the LS (verify in M2). |
| Codex | `~/.codex/sessions/**/rollout-*.jsonl.zst` (all zstd) | `codex login status` → "Logged in using ChatGPT" (auth mode only, no identity) |

> Disclosure (from Revision 2): during planning I printed only the top-level key names of `~/.codex/auth.json`. No values were printed or stored. AgentPulse never reads that file (§6.3).

---

## Changes in Revision 3

| # | Review point | Change | Why |
|---|---|---|---|
| 1 | Identity vs usage accounts | **Split** the old `accounts` table into `identities` (who is signed in) and `usage_accounts` (one provider **entitlement**: the thing a quota and usage belong to). Each agent has a declared **entitlement scope**, backed by provider evidence. Agents share a usage account only when their scope *and* identity match. Manual linking moved to non-destructive **link groups**. | One Google login can carry separate Antigravity and Gemini CLI quotas. Sharing a login is not sharing an entitlement, so the two concepts need separate models. |
| 2 | Codex credential privacy | Codex is permanently `install_scoped` with a user-editable label for MVP. `~/.codex/auth.json` is on the never-read list with a test. M2 looks only for a *supported* identity interface. Risk #2 is resolved. | The only identity source found so far is a credential file. |
| 3 | Database integrity | No nullable column in any PK/UNIQUE, using sentinel rows and `NOT NULL DEFAULT` values, enforced by a schema-introspection test. One `BEGIN IMMEDIATE` transaction per batch covers usage, rollup deltas, segments, sessions, cursor and status. Claude dedup now uses a **full token vector + `source_seq`** with an explicit delta, including moves between hour buckets. Late and duplicate handling is defined. Link and unlink never rewrite records. | SQLite treats NULLs in UNIQUE as distinct (and rowid tables even allow NULL in a PK). A partial transaction or output-only dedup would double-count. |
| 4 | Shutdown reliability | Writer intake is **closed** (receiver dropped) before late tasks can submit. Stragglers are **aborted**. Batches are submitted only as whole, self-contained units. Cursor advance commits inside the batch transaction. | A timed-out collector can't write after shutdown begins, and a crash at any point leaves a consistent DB that re-reads safely. |
| 5 | Secret memory handling | `secrecy` (`SecretString`/`SecretBox`, built on **`zeroize`**) replaces the hand-rolled `Redacted<T>`. Secrets go file buffer → `Zeroizing` → `SecretString` with no intermediate `String`. Headers are marked sensitive. Parse errors on secret-bearing or content-bearing files drop serde's message. `compile_fail` doctests prove the secret types can't be `Serialize`/`Display`. | A reviewed zeroize implementation, fewer copies, and no leaks through errors (serde_json error text can quote input values). |

<details><summary>Revision 2 changes (approved)</summary>

Identity resolver with evidence levels and no automatic merging on unverified identity · hide-to-tray with a muted event bridge and measured benchmarks (WebView destruction only if data requires it) · richer `Metric<T>` (unit, stability, timestamps, freshness, unavailable reason, estimation basis, last error) · run-length quota/context segments, hourly UTC rollups, unavailability never written as history · privacy tests per parser, log hygiene, never-read list · hardened, isolated Antigravity collector with fallback · single Tauri-owned Tokio runtime, cancellation + graceful shutdown, `panic = "unwind"`.
</details>

---

## 1. Directory structure
```
agents-pulse/
├─ AGENTPULSE_BLUEPRINT.md · CLAUDE.md
├─ docs/ ARCHITECTURE.md · perf.md · providers/<provider>.md
├─ scripts/perf/                     process-tree RSS/CPU measurement scripts
├─ src/                              React frontend
│  ├─ app/   features/{overview,accounts,sessions,analytics,settings}/ (lazy)
│  ├─ components/ (MetricValue, SourceBadge, FreshnessTag, Meter, Table)
│  ├─ ipc/ commands.ts · events.ts · bindings/ (ts-rs generated)
│  └─ store/ (useSyncExternalStore, sliced)
└─ src-tauri/                        workspace root + app crate (thin Tauri glue)
   ├─ src/ lib.rs · commands/ · events.rs · tray.rs · shutdown.rs
   └─ crates/
      ├─ core/        Metric<T>, SourceKind, identity/entitlement types, Collector trait, secret helpers, errors
      ├─ store/       writer thread, read pool, migrations, repositories, rollups, retention
      ├─ engine/      supervisor, identity/entitlement resolver, pipeline, freshness, thresholds, event bus
      └─ collectors/  antigravity/ (cargo feature) · gemini_cli/ · claude_code/ · codex/ · mock/ · testkit/
```

## 2. Frontend architecture
- **Navigation:** no router library. Sections switch on state, each loaded with `React.lazy`. Recharts loads only inside Analytics.
- **State:** a store built on `useSyncExternalStore` with per-slice selectors. All IPC goes through `ipc/`.
- **`MetricValue`** renders value + unit + `SourceBadge` + `FreshnessTag`:
  - Stale values are dimmed and labelled "as of HH:MM · stale".
  - Estimated values are dashed, with an "Estimated" badge and a tooltip showing the estimation basis.
  - Undocumented sources show "Local · unofficial".
  - Unavailable values show the reason text, never `0`.
  - Charts render missing data as gaps.
- **Accounts UI (changed):** grouped by **identity** ("Signed in as …"). Each identity lists its separate usage accounts (e.g. *Antigravity quota* and *Gemini CLI quota*), each with its own quota and usage. Linked accounts show a "linked" chip and an unlink action.

## 3. Rust backend architecture

### 3.1 Runtime model
- **One Tokio runtime: Tauri's.** The engine is spawned with `tauri::async_runtime::spawn` in `setup()`. The library crates never build a runtime; tests use `#[tokio::test]`.
- **Threads:**
  - Tauri main/UI thread.
  - Tauri Tokio worker pool (engine, collectors, event bridge).
  - **One dedicated `std::thread` SQLite writer.**
  - Read queries on `spawn_blocking`.
  - `notify` watcher thread(s), bridged to Tokio through mpsc.
- **Commands:** all `#[tauri::command]`s are `async`.
- **Failure containment:**
  - Release profile uses `panic = "unwind"`.
  - Each collector run is a task. A panic or `Err` marks that collector `Failed` and triggers jittered backoff (30s → 30 min).
  - Every `collect()` is wrapped in a timeout.
  - The writer applies each batch under `catch_unwind` and rolls back on failure.
  - `emit` errors are logged and dropped.
  - A bounded broadcast bus (cap 256) sends `engine://resync` when a receiver lags.
  - `clippy::unwrap_used` is set to warn in library crates.

### 3.2 Shutdown and cancellation (changed, review point 4)
- **Batch contract:** a collector builds a complete, self-contained `NormalizedBatch` in memory and submits it with **one** `WriterHandle::submit(batch)`. That batch includes the new cursor positions and the collector-status update. Nothing reaches the writer piecemeal, so aborting a task at any await point loses only uncommitted in-memory work.
- **Writer intake:**
  - The writer owns the only `mpsc::Receiver<WriterMsg>`.
  - `WriterHandle` holds the `Sender` plus an `Arc<AtomicBool> accepting`.
  - `submit()` returns `Err(WriterClosed)` if `accepting == false` or if the channel is closed, and the collector discards the batch.
- **Sequence:**
  1. `RunEvent::ExitRequested` → `prevent_exit()`.
  2. `cancel_token.cancel()`. Collectors `select!` on the token at every await, including `submit()`.
  3. `tracker.close(); timeout(3s, tracker.wait())`.
  4. **Close intake:** `accepting.store(false)`, then send `WriterMsg::Shutdown`. Batches already queued ahead of `Shutdown` are applied, which is safe because each is a complete atomic unit. After reading `Shutdown`, the writer **drops the receiver**, so any later send fails with `Closed` and can't write.
  5. **Abort stragglers:** call `AbortHandle::abort()` on every collector task still running (handles are kept in a `JoinSet` per supervisor). Even if one escapes abortion until process exit, step 4 guarantees it can't write.
  6. The writer finishes, runs `PRAGMA wal_checkpoint(TRUNCATE)` and closes. The thread is joined with a 2s timeout.
  7. `app.exit(0)`.
- **Consistency under hard termination:** every batch is one SQLite transaction (`BEGIN IMMEDIATE … COMMIT`). If the process is killed mid-transaction, WAL rollback discards the whole batch, *including its cursor advance*. On restart the collector re-reads from the last committed cursor, and idempotent dedup (§5.4) makes the replay exact.

### 3.3 Pipeline
`trigger` → `detect()` → identity + entitlement resolution → `collect()` → `normalize()` → `Metric` invariant validation → `submit(batch)` → writer transaction → `ChangeSet` → thresholds/alerts → event bus → bridge (coalesced ≤1/s per topic, muted while hidden).

### 3.4 `Metric<T>` (unchanged from Revision 2)
```rust
pub struct Metric<T> {
    value: Option<T>, unit: Unit, source: SourceKind, stability: Stability,
    observed_at: Option<DateTime<Utc>>, collected_at: Option<DateTime<Utc>>,
    last_success_at: Option<DateTime<Utc>>, freshness: Freshness /* computed at read time */,
    unavailable_reason: Option<UnavailableReason>, estimation_basis: Option<String>,
    last_error: Option<CollectionError>,
}
```
- **Invariants** (enforced by the `observed`/`estimated`/`unavailable` constructors):
  - `value.is_none() ⇔ source == Unavailable ⇔ reason.is_some()`
  - `Estimated ⇒ estimation_basis.is_some()`
- **Failures:** a failed collection keeps the last good value, marks it stale and sets `last_error`.

## 4. Tauri integration (unchanged)
- **Permissions:** the main window gets `core:default` plus its own commands only, under a strict CSP.
- **Close to tray:** `CloseRequested` → `prevent_close()` + `hide()`. The tray offers Show / Refresh / Quit, and the event bridge stays muted while the window is hidden.
- **Plugins:**
  - single-instance and tray at M1
  - notification at M9
  - autostart at M10, opt-in

## 5. Database

`rusqlite` (bundled) · WAL · `synchronous=NORMAL` · `foreign_keys=ON` · `busy_timeout=5000` · `user_version` migrator · timestamps **unix-ms UTC INTEGER**.

**Schema rule (new):** no column in any PRIMARY KEY or UNIQUE constraint/index may be nullable. A missing value uses a sentinel row (e.g. the `unknown` model) or `NOT NULL DEFAULT 0`. All tables with composite keys are `WITHOUT ROWID` or have explicit `NOT NULL`. A migration test walks `pragma_index_list` / `pragma_index_xinfo` / `pragma_table_info` and **fails if any key column is nullable**.

### 5.1 Identity and entitlement (changed, review point 1)
| Table | Columns | Constraints / indexes |
|---|---|---|
| `providers` | `id TEXT PK`, `display_name` | seeded |
| `agents` | `id TEXT PK`, `provider_id`, `entitlement_scope TEXT NOT NULL` | seeded from the code registry (§6.1) |
| `identities` | `id`, `provider_id`, `kind` (`verified_id` \| `verified_email`), `identity_key` (SHA-256 of `provider:stable_id`), `display_label`, `created_at` | `UNIQUE(provider_id, identity_key)`. Display grouping only: **usage never aggregates by identity.** |
| `usage_accounts` | `id`, `provider_id`, `entitlement_scope`, `account_key` (hash), `identity_id?` (FK, nullable, not in a key), `kind` (`verified` \| `install_scoped`), `display_label` (user-editable), `plan_label?`, `status`, `created_at`, `disconnected_at?` | `UNIQUE(provider_id, entitlement_scope, account_key)`. `account_key` = `H(scope:identity_key[:org])` when verified, `H(scope:agent:install_key)` when install-scoped. |
| `agent_bindings` | `id`, `agent_id`, `install_key`, `usage_account_id`, `evidence`, `bound_at`, `unbound_at?` | `UNIQUE(agent_id, install_key, bound_at)`; partial unique `(agent_id, install_key) WHERE unbound_at IS NULL` |
| `link_groups` | `id`, `display_label`, `created_at` | |
| `link_memberships` | `id`, `group_id`, `usage_account_id`, `linked_at`, `unlinked_at?` | partial unique `(usage_account_id) WHERE unlinked_at IS NULL` (an account is in at most one active group); idx `(group_id)` |

### 5.2 Usage and history
| Table | Columns | Constraints / indexes |
|---|---|---|
| `models` | `id`, `provider_id`, `name NOT NULL`, `context_window?`, `context_window_source` | `UNIQUE(provider_id, name)`; **one seeded `unknown` row per provider** |
| `sessions` | `id`, `usage_account_id NOT NULL`, `agent_id NOT NULL`, `external_session_key NOT NULL` (hashed), `model_id NOT NULL` (sentinel allowed), `started_at`, `last_activity_at`, `ended_at?`, `status` | `UNIQUE(agent_id, external_session_key)`; idx `(usage_account_id, last_activity_at DESC)`; partial idx `WHERE ended_at IS NULL` |
| `usage_records` | `id`, `usage_account_id NOT NULL`, `agent_id NOT NULL`, `session_id?`, `model_id NOT NULL` (sentinel if unknown), `occurred_at NOT NULL`, `input/output/cache_read/cache_write/reasoning_tokens` (nullable = not reported, never in a key), `source_kind`, `stability`, `dedup_key NOT NULL`, `source_seq INTEGER NOT NULL`, `collected_at` | `UNIQUE(dedup_key)`; idx `(usage_account_id, occurred_at)`, `(session_id, occurred_at)` |
| `usage_hourly` | `usage_account_id, agent_id, model_id, hour_utc` (**all NOT NULL**), summed tokens (NOT NULL DEFAULT 0), `record_count` | PK on those four · `WITHOUT ROWID` (SQLite enforces NOT NULL on WITHOUT ROWID PKs, as a second guard) |
| `quota_segments` | `id`, `usage_account_id NOT NULL`, `window_kind NOT NULL`, `used_pct`, `resets_at?`, `source_kind`, `stability`, `first_observed_at NOT NULL`, `last_observed_at NOT NULL` | `UNIQUE(usage_account_id, window_kind, first_observed_at)`; idx `(usage_account_id, window_kind, last_observed_at DESC)` |
| `context_segments` | `id`, `session_id NOT NULL`, `used_tokens`, `total_tokens?`, `total_source`, `source_kind`, `first_observed_at NOT NULL`, `last_observed_at NOT NULL` | `UNIQUE(session_id, first_observed_at)`; idx `(session_id, last_observed_at DESC)` |

### 5.3 Operational
| Table | Constraints |
|---|---|
| `metric_availability(collector_id, usage_account_id, metric, state, reason?, since)` | PK first three, all NOT NULL. Install-scoped placeholder account before identity resolves. Current state only. |
| `collector_status(collector_id PK, state, detected_version?, last_success_at?, last_error_code?, last_error_at?, consecutive_failures, next_run_at?)` | |
| `collector_cursors(collector_id, source_key, byte_offset, file_id, size, mtime)` | PK `(collector_id, source_key)`, all NOT NULL |
| `notifications(id, usage_account_id, subject, level, value, window_reset_at NOT NULL DEFAULT 0, raised_at, acked_at?)` | `UNIQUE(usage_account_id, subject, level, window_reset_at)`. `0` = no reset window. Previously nullable, which would have allowed duplicate alerts. |
| `app_settings(key PK, value JSON)` · `billing_records` deferred | |

### 5.4 Write semantics (changed, review point 3)
**One batch = one `BEGIN IMMEDIATE` transaction on the single writer thread,** applied in this order:
1. Resolve or insert `identities` / `usage_accounts` / `agent_bindings` / `sessions` / `models` (sentinel fallback).
2. Apply usage records. For each record:
   - `SELECT` the existing row by `dedup_key`.
   - **Missing** → `INSERT`. The delta is the full token vector.
   - **Exists, `incoming.source_seq <= existing.source_seq`** → no-op (duplicate or stale revision), delta = 0.
   - **Exists, `incoming.source_seq > existing.source_seq`** → `UPDATE` **all** token fields, plus `occurred_at`, `model_id`, `source_seq` and `collected_at`. The delta is `new − old` **per field**, where NULL counts as 0 for sums and a NULL→value change is a positive delta.
3. Apply rollup deltas to `usage_hourly` with `INSERT … ON CONFLICT DO UPDATE SET x = x + :dx`, skipping zero deltas.
   - If `occurred_at` or `model_id` changed between revisions, subtract the old vector from the **old** bucket and add the new vector to the **new** bucket.
   - `record_count` changes only on first insert or on a bucket move.
4. Apply segments:
   - Same value and same `resets_at` → extend `last_observed_at`.
   - Changed value or a new window → close the current segment and insert a new one.
   - An observation **older than the key's latest `last_observed_at`** is dropped and counted in metrics. Gauges are point-in-time, so a late reading must not rewrite history.
5. Upsert sessions, then `metric_availability`.
6. Advance `collector_cursors` and update `collector_status`, then `COMMIT`.

Any error → `ROLLBACK` of the whole batch (cursor included), and the collector retries on the next trigger.

**`source_seq` definition, per collector (documented in `docs/providers/*.md`):**
- Claude Code: `(file_id << 40) | byte_offset_of_line`. A later line for the same `message.id` always wins and replaces *every* token field (input, output, cache_read, cache_write).
- Codex: event index within the rollout file.
- Telemetry: the exporter sequence number or timestamp.

**Late and duplicate events:**
- Dedup is idempotent, so replays, cursor rewinds (file truncated or rotated, detected by `file_id`/size), and two triggers racing on the same file all result in a no-op or an exact correction.
- A genuinely late new event lands in its own historical hour bucket.
- Unit tests cover: replay ×2, out-of-order revisions, revision lowering `input_tokens`, NULL→value, a bucket move across hours/models, and a crash between statements (simulated by injected error → rollback → re-apply).

**Linking and attribution:**
- Raw records, sessions, segments and rollups are keyed by `usage_account_id` and **never rewritten** by link or unlink.
- Linking adds a `link_memberships` row; unlinking sets `unlinked_at`. Grouped views resolve the active group at query time, which is cheap because rollups are per account and summed at read time.
- Unlinking restores the separate views exactly.
- Login switches close the old `agent_binding`, so older records stay attributed to the account active at `occurred_at`.

**Retention defaults (configurable):**
- Raw `usage_records`: 365 days
- `context_segments`: 180 days
- `quota_segments` and `usage_hourly`: kept

`rebuild_rollups()` recomputes `usage_hourly` from raw records. It's used in tests to prove that incremental rollups equal a full recompute.

## 6. Collectors, identity, privacy

```rust
#[async_trait]
pub trait Collector: Send + Sync {
    fn id(&self) -> CollectorId;
    fn agent(&self) -> AgentId;
    fn entitlement_scope(&self) -> EntitlementScope;   // from the registry, evidence-backed
    fn capabilities(&self) -> Capabilities;
    fn triggers(&self) -> Vec<Trigger>;
    async fn detect(&self, ctx: &Ctx) -> Detection;    // installs, version, IdentityEvidence per install
    async fn collect(&self, ctx: &Ctx, cancel: &CancellationToken) -> Result<NormalizedBatch, CollectorError>;
}
```

### 6.1 Identity vs entitlement (changed, review point 1)
- **Identity** answers "who is signed in" (a Google email, an Anthropic `accountUuid`). It's used for display and grouping.
- **Entitlement scope** answers "whose quota is this", e.g. `anthropic:claude_subscription`, `google:antigravity`, `google:gemini_cli`, `openai:chatgpt_codex`.
- The scope is declared per agent in a code registry. Each entry cites its evidence in `docs/providers/*.md`, and **two agents share a scope only when provider documentation confirms a shared entitlement**. With no confirmation, each agent gets its own scope.
- **Resolver:** `(entitlement_scope, identity_key[, org])` → `usage_account`.
  - The same identity under different scopes gives separate usage accounts that point at one `identities` row. The UI groups them, but their usage and quota never combine.
  - The same scope with the same verified identity gives one usage account, so there are no duplicates.
  - No verified identity gives an `install_scoped` usage account.
- **Manual linking stays.** It's always user-initiated, reversible and non-destructive (§5.4).

| Agent | Identity source | Entitlement scope (initial, verify in M2) |
|---|---|---|
| Claude Code | `~/.claude.json → oauthAccount.accountUuid` (+ `organizationUuid`, since seat quotas are per org) | `anthropic:claude_subscription` (Claude Code is documented to draw on the Pro/Max plan). API-key mode → `anthropic:api` + install-scoped. |
| Gemini CLI | `~/.gemini/google_accounts.json → active`; auth mode from `settings.json` | `google:gemini_cli` (API-key/Vertex modes get distinct scopes) |
| Antigravity | From the LS if a user-info method exists (M2), otherwise install-scoped per install | `google:antigravity`, **separate from Gemini CLI** even with the same Google identity |
| Codex | none (see §6.3) | `openai:chatgpt_codex`, **install-scoped** |

### 6.2 Privacy
- **Never-read list** (path-allowlist unit test per collector):
  - `~/.claude/.credentials.json`, `~/.codex/auth.json`, `~/.gemini/oauth_creds.json`.
  - Antigravity `*.pb`, `brain/`, `conversations/`, `knowledge/`.
  - Gemini `tmp/` chat files.
- **Narrow serde parsers:** usage, model, opaque IDs and timestamps only. Lines are streamed from cursors and the raw buffer is dropped right after parsing. Session IDs and paths are hashed.
- **Parse errors on content-bearing or secret-bearing input** map to `CollectorError::Parse { file_kind, line }` **without** serde's message, because serde_json error text can quote input values.
- **Log hygiene:** codes, collector IDs, hashed keys and line numbers only.
- **Mandatory three-way privacy test per parser:** sentinel strings must be absent from the batch, a full DB dump and the captured logs.

### 6.3 Codex identity (changed, review point 2)
- MVP: **install-scoped** usage account (`install_key` = hash of the Codex home path) with a user-editable label, defaulting to "Codex (ChatGPT login)" based on `codex login status`.
- `~/.codex/auth.json` is never opened. It's on the never-read list, covered by the path-allowlist test, and its contents are never exposed or persisted.
- M2 investigates **supported** identity interfaces only: documented CLI output, the documented Codex app-server/SDK protocol, and official docs. Credential-file access is out of scope unless you approve it later as a separate decision.

### 6.4 Secret handling (changed, review point 5)
- Uses the **`secrecy`** crate (`SecretString`, `SecretBox<T>`), which is built on **`zeroize`**:
  - Memory is zeroed on drop.
  - `Debug` prints `[REDACTED]`.
  - There's no `Display`, and no `Serialize` unless explicitly opted in. We never opt in.
  - Access only via `expose_secret()`, which is grep-able in review.
- **Minimal copies (Antigravity CSRF):**
  1. Read the daemon file into `Zeroizing<Vec<u8>>`.
  2. Deserialize directly into a struct whose token field is `SecretString`, with no intermediate `String` field.
  3. The file buffer is zeroed on drop right after parsing.
  4. The token is re-read on each `detect()` and lives only inside the Antigravity collector, never passed to the engine, events or store.
- **HTTP header:** built once per request from `expose_secret()` into a `HeaderValue` with `set_sensitive(true)`, which keeps it out of reqwest/hyper debug output. *Known limitation:* the `HeaderValue` copy inside reqwest isn't zeroized (risk #9).
- **Errors:** secret-bearing types never appear in error variants. `CollectorError` holds codes and sanitized text only.
- **Tests:**
  - `compile_fail` doctests assert the secret types can't `serde::Serialize` or `format!("{}")`.
  - A unit test asserts `format!("{:?}")` == `[REDACTED]`.
  - A log-capture test runs the Antigravity collector against a mock LS with a sentinel token and asserts the token is absent from logs, errors, events and the DB.
- **AgentPulse-owned credentials** (future official APIs) go in the OS keychain via `keyring`. They're loaded into `SecretString` and never stored in SQLite.

## 7. Antigravity collector (unchanged except secret handling above)
- **Process and endpoint verification:**
  - Each `ls_*.json` must have a live PID.
  - The process executable path must sit inside a known Antigravity install directory (Windows `QueryFullProcessImageNameW`, macOS `proc_pidpath`).
  - The file mtime must be newer than the process start.
  - Connect to `127.0.0.1:<httpPort>` only.
- **Read-only:** method allowlist verified in M2.
- **Version gate:** a table of verified `lsVersion` values. An unknown version gets one strict-schema probe; if that fails → `UnsupportedVersion`, with a 30-min backoff.
- **Isolation:**
  - Cargo feature (can compile it out).
  - Own task, 5s timeout, own client.
  - Runtime kill switch in Settings.
- **Fallback, staged:**
  1. Keep last good values, marked stale.
  2. User-facing notice.
  3. Ship an updated version map.
  4. Switch to a documented interface if one appears.
  - Never parse protobuf state files.

## 8. React ↔ Rust communication
- **Commands:**
  - `get_overview`, `list_identities_with_accounts`, `list_sessions`, `get_session_context_history`
  - `link_accounts(account_ids, label)`, `unlink_account(id)`, `rename_account`, `disconnect_account`
  - `get_usage_series(range, granularity, tz, group_by: account | link_group | agent | model)`, `get_quota_history`
  - `get_collector_status`, `refresh`, `get_settings` / `update_settings`, `delete_local_data`
- **Events:** `engine://snapshot`, `engine://session`, `engine://collector-status`, `engine://alert`, `engine://resync`. All deltas, coalesced, and muted while the window is hidden.
- **Types:** ts-rs bindings generated from the Rust DTOs.

## 9. Background monitoring (unchanged)
- Event-driven `notify` watchers with a 2s debounce.
- Antigravity is the only poller: 60s while the LS runs, detect-only every 5 min otherwise.
- Intervals stretch about 4x while the window is hidden.
- Collectors start staggered after first paint.

## 10. Performance (unchanged)
- **Approach:** hide-to-tray with a muted bridge, watchers instead of polling, incremental parsing, one transaction per batch, segments, hourly rollups, lazy UI.
- **Benchmarks** in `docs/perf.md`:
  - Idle RSS summed over the whole process tree (WebView2 / WebKit child processes included), window visible vs hidden.
  - 10-minute idle CPU.
  - Cold start.
  - Collector run time.
  - DB growth per day.
- **Decision rule:** WebView destruction is reconsidered only if the measurements miss the target.

## 11. Milestones (structure unchanged; M1/M2 exit criteria extended)
| # | Milestone | Exit criteria |
|---|---|---|
| M0 | pnpm only, Tailwind v4, `CLAUDE.md` replaces duplicate `AGENTS.md`, this doc → `docs/ARCHITECTURE.md`, release `panic = "unwind"` | `pnpm tauri dev` launches styled |
| M1 | Workspace + 4 crates; `Metric<T>`; schema + migrator; writer with batch transactions; supervisor + mock collectors (healthy, panicking, hanging, slow-submit); identity/entitlement resolver; event round trip; hide-to-tray; graceful shutdown; perf baseline | Tests green, including: nullable-key schema check, dedup/rollup delta suite, incremental = `rebuild_rollups()`, link/unlink attribution, **post-shutdown submit returns `WriterClosed` and writes nothing**, kill-mid-transaction recovery, secret `compile_fail`/Debug tests |
| M2 | Verification spike → `docs/providers/*.md`: fields, versions, dedup key + `source_seq`, identity source, **entitlement-scope evidence**, Antigravity allowlist, supported Codex identity interface (if any) | You approve claimed metrics, identity kinds and scopes |
| M3 | Antigravity collector (or Codex/Claude Code if M2 rules it out) | Real data end to end; privacy + secret tests |
| M4–M6 | Gemini CLI, Claude Code, Codex | Same, plus dedup and identity tests |
| M7 | Accounts UI (identity grouping, link/unlink/rename) + analytics queries | No duplicates or wrong merges; local-tz series correct |
| M8 | Dashboard sections | Every metric shows source and freshness |
| M9 | Notifications | One alert per threshold per window |
| M10 | Perf pass, autostart, Windows + macOS testing, packaging | Benchmarks re-measured |

## Dependencies (Rust)
`tokio` (rt, sync, time, macros; Tauri's runtime) · `tokio-util` · `rusqlite` (bundled) · `serde`/`serde_json` · `chrono` · `thiserror` · `anyhow` (app only) · `tracing` + `tracing-subscriber` + `tracing-appender` · `async-trait` · `ts-rs` · `sha2` · **`secrecy` (serde feature for Deserialize only) + `zeroize`** · `notify` + `notify-debouncer-mini` (M3+) · `zstd` (Codex) · `reqwest` rustls-only (Antigravity) · `keyring` (when the first owned credential exists). Process lookup via `windows-sys` / `libc`.

---

## Remaining technical risks and assumptions
1. **Antigravity LS interface is unverified:** methods, quota/context/identity exposure and version stability. Resolved in M2. If unusable → Unavailable with a reason, and M3 switches provider.
2. ~~Codex identity requires credential access~~ → **Resolved:** install-scoped for MVP. Residual: a user who has two Codex homes, or switches ChatGPT accounts in one home, can't be told apart automatically. Mitigation: a manual label, plus a binding change when `codex login status` reports a change of auth mode.
3. **Subscription quota availability** for every provider must be verified in M2. Some quotas may be Unavailable.
4. **Entitlement scopes depend on provider documentation.** If a provider later documents shared quota (e.g. Antigravity and Gemini CLI), that's a registry change plus migration, and history stays intact because records aren't rewritten.
5. **Undocumented local formats** (Claude JSONL, Codex rollout) can change with any CLI update. Mitigation: schema validation → `UnsupportedVersion`.
6. **Claude streaming:** I'm assuming the later line for a `message.id` carries the most complete usage. Verify in M2. The full-vector replace + delta logic stays correct even if earlier lines have larger values.
7. **Context-window totals** are often unreported. Model-table totals are labelled Estimated.
8. **Memory:** the WebView may approach 100 MB on its own. It's measured, with destruction kept as a data-driven fallback.
9. **Secret copies outside our control:** reqwest/hyper's `HeaderValue` and kernel socket buffers aren't zeroized, and the OS may page memory to disk. `secrecy` covers our own copies only. Accepted as a residual risk for a short-lived, loopback-only token.
10. **Late gauge readings are dropped** by design (§5.4). If a provider later supplies backfilled quota history, it needs a separate import path.
11. **macOS** process-path verification and signed-app access to `~/.claude`, `~/.codex` and `~/.gemini` need testing on a Mac (M10 or earlier).
12. **Assumption:** Tauri 2's default runtime remains Tokio multi-thread. Re-check on Tauri upgrades.
13. **`panic = "unwind"`** adds a small amount of binary size. Measure it at M0.

## Verification
- **M0:** `pnpm install && pnpm tauri dev` opens a styled window. `pnpm build` passes.
- **M1:**
  - Run `cd src-tauri && cargo test --workspace`. The suite covers:
    - metric invariants
    - nullable-key schema introspection
    - the dedup/rollup delta suite (replay, out-of-order, field decreases, NULL→value, bucket moves)
    - incremental rollups equal `rebuild_rollups()`
    - segment extend/close/late-drop
    - resolver cases: same identity + different scopes → separate accounts; same scope + identity → one account; unverified → install-scoped
    - link/unlink leaves raw records untouched
    - panicking and hanging collector isolation
    - shutdown: a straggler's `submit()` after intake closes returns `WriterClosed` and the row count is unchanged
    - an injected mid-transaction failure rolls back the cursor too
    - secret `compile_fail` + Debug tests
  - Manual: live mock updates; close → tray; Quit → WAL checkpointed, `journal_mode` = `wal`; perf baseline recorded.
- **Each collector:**
  - Privacy and secret tests pass.
  - Values match the provider's own UI/CLI, with the comparison recorded in `docs/providers/<provider>.md`.
  - Killing or upgrading the provider app shows the correct stale/unavailable state.

---

## Implementation notes

### M1 (2026-11-03)
Where the implementation deliberately differs from, or refines, the text above:

1. **Entitlement scope is chosen per batch, not per collector.** `NormalizedBatch.entitlement_scope` must be one of the agent's `allowed_scopes` in `crates/core/src/registry.rs`, and the resolver rejects anything else. A collector-wide `entitlement_scope()` method couldn't express auth-mode-dependent scopes (e.g. Claude subscription vs API key).
2. **Resolver placement.** The resolver (`crates/engine/src/resolver.rs`) is pure and produces an `AccountResolution`. The store upserts exactly that inside the batch transaction (`ResolvedBatch`), so the store never interprets identity evidence.
3. **Events carry IDs, not data.** `engine://data-changed` names the collectors and usage accounts that changed, and the UI refetches those slices. This keeps payloads tiny and always consistent with SQLite.
4. **"Tokens today" reads raw records** for the local-day window, so non-whole-hour time zones are exact. Hourly rollups are used for analytics series (M7).
5. **Freshness tick comes from Rust** (`engine://freshness-tick`, every 60s, only while the window is visible). A hidden WebView2 window still reports `document.visibilityState === "visible"`, so a frontend timer kept running while hidden. This was found during M1 manual testing.
6. **Mock collectors** are compiled in (`agentpulse-collectors` feature `mock`) but registered only when `cfg(debug_assertions)`. They report under provider/agent `mock`, and the UI shows a "synthetic data" banner.
7. **Extra columns:** `usage_accounts.last_collected_at` (observation time for token metrics) and `metric_availability.source_kind/stability` (an available metric carries its declared source, so "tokens today = 0" is a real observation with a source, never a placeholder).
8. **Same-timestamp conflicting gauge readings:** the first one wins and the later one is counted as dropped. This extends the "late readings are dropped" rule.
9. **`secrecy` deserialization path:** `Deserialize for SecretString` goes through a `String` and then `into_boxed_str`. serde_json allocates exact-size strings, so this shouldn't reallocate, but it must be re-verified in M3 when the Antigravity token parser is written.

### Collector opt-in and per-tool cards (2026-11-03, requested after M1)
- **Collectors are opt-in.** Nothing runs, and nothing is read from an AI tool, until the user enables it. The enabled set is persisted in `app_settings` under `collectors.enabled` (JSON array of collector ids) and restored at startup (`Engine::enabled_from_settings`).
- **The engine starts and stops workers at runtime** (`Engine::set_enabled` / `set_all_enabled`). Each worker has its own cancellation token (a child of the engine token). Toggles are serialized so the persisted set always matches the running set.
- **Disabling** stops the worker, sets `collector_status.state = 'disabled'`, and flips the collector's `metric_availability` rows to unavailable with reason `collector_disabled`. History is untouched, but old quotas and today's per-model breakdown are no longer presented as current.
- **Dashboard = one card per AI tool** (`get_dashboard` → `DashboardDto`). Each card has its own Enable/Disable, the bound usage accounts, and today's per-model token breakdown. The header has Enable all / Disable all, covering only tools that have a collector in this build. Tools without a collector show their planned milestone and an inactive button.
- The M1 `get_overview` / `get_collector_status` commands were replaced by `get_dashboard`, `set_collector_enabled(agent, enabled)` and `set_all_collectors_enabled(enabled)`.

### M1 close-out (2026-11-04)
- **Release builds suppress WebView2's browser context menu** (Refresh, Save as, Print, Inspect) via `src/app/contextMenu.ts`. It still appears on selected text and in editable fields, so Copy and Paste keep working. Dev builds keep the full menu.
- **Memory over the 100 MB target is deferred to M10.** Risk #13 is resolved: `panic = "unwind"` is kept at a measured +4.4 MB. See `docs/perf.md`.

### M4: Claude Code collector (2026-11-06)
1. **Read-only rule.** AgentPulse never writes to, reconfigures or runs an AI tool in a way that changes its files. This drops the Claude status line tap and the Gemini telemetry edit (`docs/providers/README.md`). A collector test asserts that the tool folders are byte-for-byte unchanged after collecting.
2. **§6.1 change:** Claude identity comes from `.claude.json → oauthAccount.accountUuid` (+ `organizationUuid`), the source this document originally planned. It is **not** `claude auth status --json` as M2 proposed, because running the CLI writes to the config folder.
3. **One config folder = one installation.** A collector may now return several batches per run, one per installation, each with its own identity. Claude Code reads `~/.claude`, `CLAUDE_CONFIG_DIR` (if visible to the app) and user-added folders (`app_settings.claude_code.added_folders`). Matching `.claude-*` folders are suggested in the UI, never added silently.
4. **`Engine::restart(collector_id)`** restarts a running worker so it re-reads its triggers (watch paths) after a configuration change. Availability and history are untouched.
5. **New `UnavailableReason::RequiresToolSettingsChange`**, for metrics a provider exposes only after its own settings are changed. It's distinct from `OptInRequired`, which is an opt-in inside AgentPulse.
6. **Context totals for Claude are Estimated** (200k, or 1M once usage passes 200k). The used value is Local.

### M4b: Claude Code quota tap (2026-11-06)
1. **The one exception to the read-only rule:** `collectors::claude_code::settings_edit` adds or removes AgentPulse's `statusLine` block in a Claude Code folder's `settings.json`. It runs only from the `set_quota_tap` command after the user confirms a `preview_quota_tap` preview. The preview's file content is sent back, so nothing is written if the file changed meanwhile.
2. **Tap mode in the main binary** (`--statusline-tap <folder key>`, handled in `main()` before Tauri) instead of a sidecar binary: no packaging changes, and the same executable path works in dev and release. Snapshots live in `<app data>/statusline/`, computed from the environment plus `APP_IDENTIFIER` (a test checks it against `tauri.conf.json`).
3. **No new dependencies** (`thiserror` and `memchr` were already in the tree). `serde_json/preserve_order` was deliberately **not** enabled: settings edits are text edits verified by parsing, so the user's file is never re-serialized.
4. **Claude Code supports Pro, Max and Ultra only** (user decision, 2026-11-06). Other plans' folders are skipped by the collector. `AdminOp::ForgetInstall` is the store's only deleting operation. It runs only from the user's explicit "Remove from AgentPulse", and it deletes only accounts bound solely to that installation.
