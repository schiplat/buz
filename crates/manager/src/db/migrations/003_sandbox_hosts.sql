-- Sandbox host configuration (design record:
-- docs/content/docs/01-architecture/execution/sandbox-host-configuration.md).
-- Manager-managed policy for buz-sandboxd instances. The config document is
-- keyed by (host_id, instance_id) — never by runner_id — and versioned.

-- One enrolled sandboxd instance per row. token_hash is a SHA-256 of the
-- opaque bearer token sandboxd uses for config fetches (never the plaintext).
CREATE TABLE IF NOT EXISTS sandbox_hosts (
    host_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    max_resource_slots INTEGER NOT NULL DEFAULT 0,
    desired_version INTEGER NOT NULL DEFAULT 0,
    applied_version INTEGER NOT NULL DEFAULT 0,
    applied_sha256 TEXT NOT NULL DEFAULT '',
    applied_at TEXT,
    last_fetch_at TEXT,
    last_fetch_error TEXT,
    consecutive_fetch_failures INTEGER NOT NULL DEFAULT 0,
    token_hash TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (host_id, instance_id)
);
CREATE INDEX IF NOT EXISTS idx_sandbox_hosts_host ON sandbox_hosts(host_id);

-- Immutable version history of desired documents. The latest row (max
-- version) is what long-poll serves, and older rows exist for audit and
-- rollback.
CREATE TABLE IF NOT EXISTS sandbox_host_configs (
    host_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    document TEXT NOT NULL,
    document_sha256 TEXT NOT NULL,
    created_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (host_id, instance_id, version)
);

-- Two-person approval queue for config changes (opt-in via platform setting).
CREATE TABLE IF NOT EXISTS sandbox_config_changes (
    id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    requested_by TEXT NOT NULL,
    approved_by TEXT,
    before_document TEXT NOT NULL DEFAULT '',
    after_document TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    decided_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_sandbox_config_changes_status
    ON sandbox_config_changes(status);

-- Team allowlist for non-default resource profiles (`default` is implicitly
-- available to every team and never needs a row.
CREATE TABLE IF NOT EXISTS team_resource_grants (
    team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    profile TEXT NOT NULL,
    granted_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (team_id, profile)
);
