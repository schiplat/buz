-- Step template library (ADR §6.2, 2026-10-04: DB is the primary trusted
-- source). raw_yaml is the truth and content_sha256 binds to it. Rows are
-- append-only — sync/import never mutates an existing (id, version).
CREATE TABLE IF NOT EXISTS step_templates (
    id TEXT NOT NULL,
    version TEXT NOT NULL,
    raw_yaml TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (id, version)
);
