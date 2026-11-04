-- AgentPulse schema v1 (architecture §5).
-- Rules:
--   * All timestamps are unix milliseconds, UTC (INTEGER).
--   * No column in any PRIMARY KEY or UNIQUE constraint/index may be nullable
--     (enforced by a schema-introspection test).
--   * Unavailability is current state only (metric_availability) and is never written to history tables.

-- ---------- registry (seeded from code at startup) ----------
CREATE TABLE providers (
    id           TEXT PRIMARY KEY NOT NULL,
    display_name TEXT NOT NULL
) WITHOUT ROWID;

CREATE TABLE agents (
    id                        TEXT PRIMARY KEY NOT NULL,
    provider_id               TEXT NOT NULL REFERENCES providers(id),
    display_name              TEXT NOT NULL,
    default_entitlement_scope TEXT NOT NULL
) WITHOUT ROWID;

-- ---------- identity vs usage accounts ----------
-- Who is signed in. Display grouping only: usage never aggregates by identity.
CREATE TABLE identities (
    id            INTEGER PRIMARY KEY,
    provider_id   TEXT NOT NULL REFERENCES providers(id),
    kind          TEXT NOT NULL CHECK (kind IN ('verified_id', 'verified_email')),
    identity_key  TEXT NOT NULL,          -- sha256(provider, normalized stable value)
    display_label TEXT,
    created_at    INTEGER NOT NULL,
    UNIQUE (provider_id, identity_key)
);

-- One provider entitlement: the thing quota and usage belong to.
CREATE TABLE usage_accounts (
    id                INTEGER PRIMARY KEY,
    provider_id       TEXT NOT NULL REFERENCES providers(id),
    entitlement_scope TEXT NOT NULL,
    account_key       TEXT NOT NULL,      -- hash(scope, identity_key[, org]) or hash(scope, agent, install_key)
    identity_id       INTEGER REFERENCES identities(id),
    kind              TEXT NOT NULL CHECK (kind IN ('verified', 'install_scoped')),
    display_label     TEXT NOT NULL,
    label_user_set    INTEGER NOT NULL DEFAULT 0,
    plan_label        TEXT,
    status            TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disconnected')),
    created_at        INTEGER NOT NULL,
    last_collected_at INTEGER,
    disconnected_at   INTEGER,
    UNIQUE (provider_id, entitlement_scope, account_key)
);
CREATE INDEX idx_usage_accounts_identity ON usage_accounts(identity_id);

-- Which usage account an agent installation was bound to, over time.
CREATE TABLE agent_bindings (
    id               INTEGER PRIMARY KEY,
    agent_id         TEXT NOT NULL REFERENCES agents(id),
    install_key      TEXT NOT NULL,
    usage_account_id INTEGER NOT NULL REFERENCES usage_accounts(id),
    evidence         TEXT NOT NULL,
    bound_at         INTEGER NOT NULL,
    unbound_at       INTEGER,
    UNIQUE (agent_id, install_key, bound_at)
);
CREATE UNIQUE INDEX uq_agent_bindings_active ON agent_bindings(agent_id, install_key) WHERE unbound_at IS NULL;
CREATE INDEX idx_agent_bindings_account ON agent_bindings(usage_account_id);

-- User-initiated, reversible, non-destructive linking.
CREATE TABLE link_groups (
    id            INTEGER PRIMARY KEY,
    display_label TEXT NOT NULL,
    created_at    INTEGER NOT NULL
);

CREATE TABLE link_memberships (
    id               INTEGER PRIMARY KEY,
    group_id         INTEGER NOT NULL REFERENCES link_groups(id),
    usage_account_id INTEGER NOT NULL REFERENCES usage_accounts(id),
    linked_at        INTEGER NOT NULL,
    unlinked_at      INTEGER
);
CREATE UNIQUE INDEX uq_link_memberships_active ON link_memberships(usage_account_id) WHERE unlinked_at IS NULL;
CREATE INDEX idx_link_memberships_group ON link_memberships(group_id);

-- ---------- usage and history ----------
CREATE TABLE models (
    id                    INTEGER PRIMARY KEY,
    provider_id           TEXT NOT NULL REFERENCES providers(id),
    name                  TEXT NOT NULL,   -- 'unknown' sentinel row per provider
    context_window        INTEGER,
    context_window_source TEXT,
    UNIQUE (provider_id, name)
);

CREATE TABLE sessions (
    id                   INTEGER PRIMARY KEY,
    usage_account_id     INTEGER NOT NULL REFERENCES usage_accounts(id),
    agent_id             TEXT NOT NULL REFERENCES agents(id),
    external_session_key TEXT NOT NULL,   -- hashed
    model_id             INTEGER NOT NULL REFERENCES models(id),
    started_at           INTEGER NOT NULL,
    last_activity_at     INTEGER NOT NULL,
    ended_at             INTEGER,
    status               TEXT NOT NULL,
    UNIQUE (agent_id, external_session_key)
);
CREATE INDEX idx_sessions_account_activity ON sessions(usage_account_id, last_activity_at DESC);
CREATE INDEX idx_sessions_open ON sessions(last_activity_at) WHERE ended_at IS NULL;

CREATE TABLE usage_records (
    id                 INTEGER PRIMARY KEY,
    usage_account_id   INTEGER NOT NULL REFERENCES usage_accounts(id),
    agent_id           TEXT NOT NULL REFERENCES agents(id),
    session_id         INTEGER REFERENCES sessions(id),
    model_id           INTEGER NOT NULL REFERENCES models(id),
    occurred_at        INTEGER NOT NULL,
    input_tokens       INTEGER,           -- NULL = not reported (distinct from 0)
    output_tokens      INTEGER,
    cache_read_tokens  INTEGER,
    cache_write_tokens INTEGER,
    reasoning_tokens   INTEGER,
    source_kind        TEXT NOT NULL,
    stability          TEXT NOT NULL,
    dedup_key          TEXT NOT NULL UNIQUE,
    source_seq         INTEGER NOT NULL,
    collected_at       INTEGER NOT NULL
);
CREATE INDEX idx_usage_records_account_time ON usage_records(usage_account_id, occurred_at);
CREATE INDEX idx_usage_records_session_time ON usage_records(session_id, occurred_at);

CREATE TABLE usage_hourly (
    usage_account_id   INTEGER NOT NULL REFERENCES usage_accounts(id),
    agent_id           TEXT NOT NULL,
    model_id           INTEGER NOT NULL,
    hour_utc           INTEGER NOT NULL,
    input_tokens       INTEGER NOT NULL DEFAULT 0,
    output_tokens      INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens   INTEGER NOT NULL DEFAULT 0,
    record_count       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (usage_account_id, agent_id, model_id, hour_utc)
) WITHOUT ROWID;

-- Run-length history: one row per distinct value; last_observed_at extends while unchanged.
CREATE TABLE quota_segments (
    id                INTEGER PRIMARY KEY,
    usage_account_id  INTEGER NOT NULL REFERENCES usage_accounts(id),
    window_kind       TEXT NOT NULL,
    used_pct          REAL NOT NULL,
    resets_at         INTEGER,
    source_kind       TEXT NOT NULL,
    stability         TEXT NOT NULL,
    first_observed_at INTEGER NOT NULL,
    last_observed_at  INTEGER NOT NULL,
    UNIQUE (usage_account_id, window_kind, first_observed_at)
);
CREATE INDEX idx_quota_segments_latest ON quota_segments(usage_account_id, window_kind, last_observed_at DESC);

CREATE TABLE context_segments (
    id                INTEGER PRIMARY KEY,
    session_id        INTEGER NOT NULL REFERENCES sessions(id),
    used_tokens       INTEGER NOT NULL,
    total_tokens      INTEGER,
    total_source      TEXT NOT NULL,
    source_kind       TEXT NOT NULL,
    stability         TEXT NOT NULL,
    first_observed_at INTEGER NOT NULL,
    last_observed_at  INTEGER NOT NULL,
    UNIQUE (session_id, first_observed_at)
);
CREATE INDEX idx_context_segments_latest ON context_segments(session_id, last_observed_at DESC);

-- ---------- operational ----------
CREATE TABLE metric_availability (
    collector_id     TEXT NOT NULL,
    usage_account_id INTEGER NOT NULL REFERENCES usage_accounts(id),
    metric           TEXT NOT NULL,
    state            TEXT NOT NULL CHECK (state IN ('available', 'unavailable')),
    source_kind      TEXT,
    stability        TEXT,
    reason           TEXT,              -- JSON-encoded UnavailableReason
    since            INTEGER NOT NULL,
    PRIMARY KEY (collector_id, usage_account_id, metric)
) WITHOUT ROWID;

CREATE TABLE collector_status (
    collector_id         TEXT PRIMARY KEY NOT NULL,
    state                TEXT NOT NULL,
    detected_version     TEXT,
    last_success_at      INTEGER,
    last_error_code      TEXT,
    last_error_message   TEXT,
    last_error_at        INTEGER,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    next_run_at          INTEGER,
    updated_at           INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE collector_cursors (
    collector_id TEXT NOT NULL,
    source_key   TEXT NOT NULL,
    byte_offset  INTEGER NOT NULL,
    file_id      TEXT NOT NULL,
    size         INTEGER NOT NULL,
    mtime        INTEGER NOT NULL,
    PRIMARY KEY (collector_id, source_key)
) WITHOUT ROWID;

CREATE TABLE notifications (
    id               INTEGER PRIMARY KEY,
    usage_account_id INTEGER NOT NULL REFERENCES usage_accounts(id),
    subject          TEXT NOT NULL,     -- 'quota:<window>' | 'context:<session>'
    level            TEXT NOT NULL,
    value            REAL NOT NULL,
    window_reset_at  INTEGER NOT NULL DEFAULT 0,  -- 0 = no reset window
    raised_at        INTEGER NOT NULL,
    acked_at         INTEGER,
    UNIQUE (usage_account_id, subject, level, window_reset_at)
);

CREATE TABLE app_settings (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL                 -- JSON
) WITHOUT ROWID;
