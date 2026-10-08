-- Step marketplace provenance (ADR §6.4): one row per resolved `uses:` task.
-- Resolution records are replaced in the same transaction as pipeline tasks.
CREATE TABLE IF NOT EXISTS step_resolutions (
    id TEXT PRIMARY KEY,
    pipeline_id TEXT NOT NULL,
    step_order INTEGER NOT NULL DEFAULT 0,
    uses_ref TEXT NOT NULL,
    resolved_version TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    source TEXT NOT NULL,
    expanded_json TEXT NOT NULL,
    input_defs_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
