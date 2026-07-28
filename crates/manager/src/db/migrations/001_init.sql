-- Buz v0.1.0 - Phase 1 Initial Schema

CREATE TABLE IF NOT EXISTS runners (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    register_token TEXT NOT NULL,
    token TEXT,
    labels TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'offline',
    version TEXT,
    system_info TEXT NOT NULL DEFAULT '{}',
    is_frozen INTEGER NOT NULL DEFAULT 0,
    last_seen TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS hosts (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    host TEXT NOT NULL,
    port INTEGER NOT NULL DEFAULT 22,
    username TEXT NOT NULL,
    credential_type TEXT NOT NULL DEFAULT 'vault',
    credential_data TEXT NOT NULL DEFAULT '',
    credential_passphrase TEXT NOT NULL DEFAULT '',
    labels TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS pipelines (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    trigger_type TEXT NOT NULL DEFAULT 'manual',
    webhook_path TEXT UNIQUE,
    cron_expr TEXT,
    runner_labels TEXT NOT NULL DEFAULT '[]',
    timeout_seconds INTEGER NOT NULL DEFAULT 3600,
    source TEXT NOT NULL DEFAULT 'form',
    yaml_path TEXT,
    concurrency_group TEXT,
    input_defs TEXT NOT NULL DEFAULT '[]',
    is_active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    pipeline_id TEXT NOT NULL REFERENCES pipelines(id) ON DELETE CASCADE,
    step_order INTEGER NOT NULL,
    name TEXT NOT NULL,
    exec_type TEXT NOT NULL DEFAULT 'local',
    host_id TEXT REFERENCES hosts(id),
    host_names TEXT NOT NULL DEFAULT '',
    host_labels TEXT NOT NULL DEFAULT '',
    command_template TEXT NOT NULL,
    work_dir TEXT NOT NULL DEFAULT '',
    retry_count INTEGER NOT NULL DEFAULT 0,
    timeout_seconds INTEGER NOT NULL DEFAULT 3600,
    fail_strategy TEXT NOT NULL DEFAULT 'stop',
    step_secrets TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_tasks_pipeline ON tasks(pipeline_id, step_order);

CREATE TABLE IF NOT EXISTS variables (
    id TEXT PRIMARY KEY,
    pipeline_id TEXT NOT NULL REFERENCES pipelines(id) ON DELETE CASCADE,
    var_name TEXT NOT NULL,
    source_type TEXT NOT NULL DEFAULT 'vault',
    vault_path TEXT,
    is_secret INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(pipeline_id, var_name)
);

CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY,
    pipeline_id TEXT NOT NULL REFERENCES pipelines(id),
    runner_id TEXT REFERENCES runners(id),
    status TEXT NOT NULL DEFAULT 'pending',
    trigger_by TEXT NOT NULL DEFAULT 'manual',
    trigger_params TEXT,
    snapshot TEXT NOT NULL DEFAULT '{}',
    step_statuses TEXT NOT NULL DEFAULT '[]',
    start_time TEXT,
    end_time TEXT,
    retry_times INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_jobs_pipeline ON jobs(pipeline_id);
CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status);
CREATE INDEX IF NOT EXISTS idx_jobs_runner ON jobs(runner_id);

CREATE TABLE IF NOT EXISTS job_logs (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    chunk_index INTEGER NOT NULL,
    step_order INTEGER NOT NULL DEFAULT 0,
    log_content TEXT NOT NULL,
    is_masked INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(job_id, chunk_index)
);
CREATE INDEX IF NOT EXISTS idx_job_logs_job ON job_logs(job_id, chunk_index)
