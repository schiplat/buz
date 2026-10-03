-- Step marketplace provenance (ADR §6.4): one row per pipeline save that
-- resolved `uses:` references (resolve & lock — upstream tag moves never
-- change an already-saved pipeline). Jobs audit which exact script ran.
CREATE TABLE IF NOT EXISTS step_resolutions (
    id TEXT PRIMARY KEY,
    pipeline_id TEXT NOT NULL,
    uses_ref TEXT NOT NULL,
    resolved_version TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    source TEXT NOT NULL,
    expanded_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_step_resolutions_pipeline ON step_resolutions(pipeline_id);
