
-- Buz Manager: Postgres baseline schema.
-- Derived mechanically from the fully migrated SQLite template database
-- (38 application tables, indexes, 2 seq triggers; `schema_migrations` is the
-- migration runner's own). TEXT columns are kept faithful —
-- timestamp comparisons in the shared SQL strings rely on text ordering,
-- and the datetime() shims produce SQLite's exact text format.
-- ───────────────────────── dialect shim functions ─────────────────────────
-- SQLite's datetime(...) calls appear verbatim in the shared SQL strings.
-- These shims make them valid Postgres with identical text output
-- ("YYYY-MM-DD HH24:MI:SS"), which every timestamp comparison in the shared
-- statements relies on (columns stay TEXT).
--
-- PG accepts 'now' as a timestamptz input, so `datetime('now')` reaches the
-- one-argument shim directly — the statement rewriter needs no datetime rule.

CREATE OR REPLACE FUNCTION datetime() RETURNS text LANGUAGE sql
    AS $$ SELECT to_char(now(), 'YYYY-MM-DD HH24:MI:SS') $$;

CREATE OR REPLACE FUNCTION datetime(value text) RETURNS text LANGUAGE sql
    AS $$ SELECT to_char(value::timestamptz, 'YYYY-MM-DD HH24:MI:SS') $$;

CREATE OR REPLACE FUNCTION datetime(value text, modifier text) RETURNS text
    LANGUAGE sql AS $$
    SELECT CASE
        WHEN modifier LIKE '-%' THEN
            to_char(value::timestamptz - modifier::interval, 'YYYY-MM-DD HH24:MI:SS')
        ELSE
            to_char(value::timestamptz + modifier::interval, 'YYYY-MM-DD HH24:MI:SS')
    END
$$;

-- ───────────────────────────── tables (FK order) ─────────────────────
-- `schema_migrations` is NOT part of the baseline: the migration runner owns
-- it (creates it before applying versions and records the row itself).
-- table manager_meta
CREATE TABLE manager_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- table hosts
CREATE TABLE hosts (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    host TEXT NOT NULL,
    port BIGINT NOT NULL DEFAULT 22,
    username TEXT NOT NULL,
    credential_type TEXT NOT NULL DEFAULT 'vault',
    credential_data TEXT NOT NULL DEFAULT '',
    credential_passphrase TEXT NOT NULL DEFAULT '',
    labels TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT datetime(),
    updated_at TEXT NOT NULL DEFAULT datetime()
, team_id TEXT);
-- table pipelines
CREATE TABLE pipelines (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    trigger_type TEXT NOT NULL DEFAULT 'manual',
    webhook_path TEXT UNIQUE,
    webhook_secret TEXT NOT NULL DEFAULT '',
    cron_expr TEXT,
    runner_labels TEXT NOT NULL DEFAULT '[]',
    timeout_seconds BIGINT NOT NULL DEFAULT 3600,
    source TEXT NOT NULL DEFAULT 'form',
    yaml_path TEXT,
    concurrency_group TEXT,
    input_defs TEXT NOT NULL DEFAULT '[]',
    is_active BIGINT NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT datetime(),
    updated_at TEXT NOT NULL DEFAULT datetime()
, yaml_sha256 TEXT NOT NULL DEFAULT '', cron_last_fired_at TEXT NOT NULL DEFAULT '', resource_profile TEXT NOT NULL DEFAULT '', owner_group TEXT NOT NULL DEFAULT '', environments TEXT NOT NULL DEFAULT '[]', notify_json TEXT NOT NULL DEFAULT '[]', webhook_environment TEXT NOT NULL DEFAULT '', cron_environment TEXT NOT NULL DEFAULT '', run_counter BIGINT NOT NULL DEFAULT 0, team_id TEXT NOT NULL DEFAULT '', concurrency_mode TEXT NOT NULL DEFAULT '', concurrency_max_queue BIGINT NOT NULL DEFAULT 3, concurrency_queue_timeout_seconds BIGINT NOT NULL DEFAULT 1800, yaml_source TEXT NOT NULL DEFAULT '');
-- table tasks
CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    pipeline_id TEXT NOT NULL REFERENCES pipelines(id) ON DELETE CASCADE,
    step_order BIGINT NOT NULL,
    name TEXT NOT NULL,
    exec_type TEXT NOT NULL DEFAULT 'shell',
    host_id TEXT REFERENCES hosts(id),
    host_names TEXT NOT NULL DEFAULT '',
    host_labels TEXT NOT NULL DEFAULT '',
    command_template TEXT NOT NULL,
    image TEXT NOT NULL DEFAULT '',
    work_dir TEXT NOT NULL DEFAULT '',
    volumes TEXT NOT NULL DEFAULT '',
    retry_count BIGINT NOT NULL DEFAULT 0,
    timeout_seconds BIGINT NOT NULL DEFAULT 3600,
    fail_strategy TEXT NOT NULL DEFAULT 'stop',
    step_secrets TEXT NOT NULL DEFAULT '',
    step_env TEXT NOT NULL DEFAULT '',
    registry_secret TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT datetime()
, privileged BIGINT NOT NULL DEFAULT 0, capabilities TEXT NOT NULL DEFAULT '[]', config_source TEXT NOT NULL DEFAULT '', config_key_path TEXT NOT NULL DEFAULT '', config_dest TEXT NOT NULL DEFAULT '', config_render TEXT NOT NULL DEFAULT '', needs_json TEXT NOT NULL DEFAULT '[]', hosts_strategy TEXT NOT NULL DEFAULT 'chain', hosts_max_parallel BIGINT);
-- table variables
CREATE TABLE variables (
    id TEXT PRIMARY KEY,
    pipeline_id TEXT NOT NULL REFERENCES pipelines(id) ON DELETE CASCADE,
    var_name TEXT NOT NULL,
    source_type TEXT NOT NULL DEFAULT 'vault',
    vault_path TEXT,
    is_secret BIGINT NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT datetime(), value TEXT NOT NULL DEFAULT '',
    UNIQUE(pipeline_id, var_name)
);
-- table step_resolutions
CREATE TABLE step_resolutions (
    id TEXT PRIMARY KEY,
    pipeline_id TEXT NOT NULL,
    step_order BIGINT NOT NULL DEFAULT 0,
    uses_ref TEXT NOT NULL,
    resolved_version TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    source TEXT NOT NULL,
    expanded_json TEXT NOT NULL,
    input_defs_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT datetime()
, reviewed BIGINT NOT NULL DEFAULT 0);
-- table sandbox_hosts
CREATE TABLE sandbox_hosts (
    host_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    max_resource_slots BIGINT NOT NULL DEFAULT 0,
    desired_version BIGINT NOT NULL DEFAULT 0,
    applied_version BIGINT NOT NULL DEFAULT 0,
    applied_sha256 TEXT NOT NULL DEFAULT '',
    applied_at TEXT,
    last_fetch_at TEXT,
    last_fetch_error TEXT,
    consecutive_fetch_failures BIGINT NOT NULL DEFAULT 0,
    token_hash TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT datetime(),
    updated_at TEXT NOT NULL DEFAULT datetime(),
    PRIMARY KEY (host_id, instance_id)
);
-- table sandbox_host_configs
CREATE TABLE sandbox_host_configs (
    host_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    version BIGINT NOT NULL,
    document TEXT NOT NULL,
    document_sha256 TEXT NOT NULL,
    created_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT datetime(),
    PRIMARY KEY (host_id, instance_id, version)
);
-- table sandbox_config_changes
CREATE TABLE sandbox_config_changes (
    id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    requested_by TEXT NOT NULL,
    approved_by TEXT,
    before_document TEXT NOT NULL DEFAULT '',
    after_document TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT datetime(),
    decided_at TEXT
, decision_comment TEXT);
-- table secrets
CREATE TABLE secrets (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            value TEXT NOT NULL DEFAULT '',
            owner_group TEXT NOT NULL DEFAULT '',
            team_shared BIGINT NOT NULL DEFAULT 0,
            kind TEXT NOT NULL DEFAULT 'generic',
            updated_at TEXT NOT NULL DEFAULT datetime()
        , team_id TEXT NOT NULL DEFAULT '');
-- table platform_vars
CREATE TABLE platform_vars (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            value TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL DEFAULT datetime()
        );
-- table pipeline_vars
CREATE TABLE pipeline_vars (
            pipeline_id TEXT NOT NULL REFERENCES pipelines(id) ON DELETE CASCADE,
            var_id TEXT NOT NULL REFERENCES platform_vars(id) ON DELETE CASCADE,
            UNIQUE(pipeline_id, var_id)
        );
-- table users
CREATE TABLE users (
            id TEXT PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            display_name TEXT NOT NULL DEFAULT '',
            is_platform_admin BIGINT NOT NULL DEFAULT 0,
            is_active BIGINT NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT datetime(),
            updated_at TEXT NOT NULL DEFAULT datetime()
        , is_super_admin BIGINT NOT NULL DEFAULT 0, is_ops BIGINT NOT NULL DEFAULT 0);
-- table teams
CREATE TABLE teams (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE,
            description TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT datetime(),
            updated_at TEXT NOT NULL DEFAULT datetime()
        );
-- table team_members
CREATE TABLE team_members (
            team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
            user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            role TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT datetime(),
            PRIMARY KEY (team_id, user_id)
        );
-- table plugin_sources
CREATE TABLE plugin_sources (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            url TEXT NOT NULL,
            auto_enable BIGINT NOT NULL DEFAULT 0,
            enabled BIGINT NOT NULL DEFAULT 1,
            last_synced_at TEXT,
            last_sync_error TEXT,
            created_by TEXT,
            created_at TEXT NOT NULL DEFAULT datetime()
        , mirror_url TEXT, last_index_sequence BIGINT, last_index_sha256 TEXT, last_index_issued_at TEXT);
-- table plugin_pool
CREATE TABLE plugin_pool (
            namespace TEXT NOT NULL,
            plugin_id TEXT NOT NULL,
            version TEXT NOT NULL,
            sha256 TEXT NOT NULL,
            source_id TEXT NOT NULL REFERENCES plugin_sources(id) ON DELETE CASCADE,
            file_path TEXT NOT NULL,
            enabled BIGINT NOT NULL DEFAULT 0,
            reviewed BIGINT NOT NULL DEFAULT 0,
            added_by TEXT,
            added_at TEXT NOT NULL DEFAULT datetime(),
            requires_buz TEXT,
            buz_compatible BIGINT,
            compat_error TEXT,
            compat_checked_against TEXT,
            meta_json TEXT, updated_by TEXT, updated_at TEXT,
            UNIQUE(namespace, plugin_id, version)
        );
-- table platform_settings
CREATE TABLE platform_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_by TEXT,
            updated_at TEXT NOT NULL DEFAULT datetime()
        );
-- table scheduler_lease
CREATE TABLE scheduler_lease (
            id BIGINT PRIMARY KEY,
            instance_id TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT datetime()
        );
-- table pipeline_git_hooks
CREATE TABLE pipeline_git_hooks (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            label TEXT NOT NULL DEFAULT '',
            provider TEXT NOT NULL,
            hook_key TEXT NOT NULL UNIQUE,
            secret_ciphertext TEXT NOT NULL,
            branch_filter TEXT NOT NULL DEFAULT '',
            environment TEXT NOT NULL DEFAULT '',
            enabled BIGINT NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT datetime(),
            updated_at TEXT NOT NULL DEFAULT datetime(), event_filter TEXT NOT NULL DEFAULT 'push',
            FOREIGN KEY (pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        );
-- table pipeline_trigger_events
CREATE TABLE pipeline_trigger_events (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            source TEXT NOT NULL,
            provider TEXT,
            event_type TEXT NOT NULL DEFAULT '',
            delivery_id TEXT,
            repo_full_name TEXT NOT NULL DEFAULT '',
            git_ref TEXT NOT NULL DEFAULT '',
            branch TEXT NOT NULL DEFAULT '',
            tag TEXT NOT NULL DEFAULT '',
            commit_sha TEXT NOT NULL DEFAULT '',
            commit_message TEXT NOT NULL DEFAULT '',
            sender TEXT NOT NULL DEFAULT '',
            compare_url TEXT NOT NULL DEFAULT '',
            received_at TEXT NOT NULL DEFAULT datetime(),
            decision TEXT NOT NULL,
            skip_reason TEXT NOT NULL DEFAULT '',
            job_id TEXT,
            created_at TEXT NOT NULL DEFAULT datetime(),
            FOREIGN KEY (pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        );
-- table audit_logs
CREATE TABLE audit_logs (
            id TEXT PRIMARY KEY,
            created_at TEXT NOT NULL DEFAULT datetime(),
            actor_user_id TEXT NOT NULL DEFAULT '',
            actor_username TEXT NOT NULL DEFAULT '',
            action TEXT NOT NULL,
            resource_type TEXT NOT NULL DEFAULT '',
            resource_id TEXT NOT NULL DEFAULT '',
            resource_name TEXT NOT NULL DEFAULT '',
            team_id TEXT,
            before_json TEXT,
            after_json TEXT,
            meta_json TEXT,
            ip TEXT NOT NULL DEFAULT ''
        , seq BIGINT NOT NULL DEFAULT 0);
-- table api_tokens
CREATE TABLE api_tokens (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            name TEXT NOT NULL,
            token_hash TEXT NOT NULL,
            token_prefix TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT datetime(),
            last_used_at TEXT,
            revoked_at TEXT, scope TEXT NOT NULL DEFAULT 'full',
            FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
        );
-- table github_connections
CREATE TABLE github_connections (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            api_base_url TEXT NOT NULL DEFAULT 'https://api.github.com',
            token_ciphertext TEXT NOT NULL,
            fetch_owners TEXT NOT NULL DEFAULT '',
            fetch_interval_minutes BIGINT,
            last_fetch_at TEXT,
            created_at TEXT NOT NULL DEFAULT datetime(),
            updated_at TEXT NOT NULL DEFAULT datetime()
        );
-- table github_repos
CREATE TABLE github_repos (
            id TEXT PRIMARY KEY,
            connection_id TEXT NOT NULL REFERENCES github_connections(id) ON DELETE CASCADE,
            owner TEXT NOT NULL,
            repo TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT datetime(),
            updated_at TEXT NOT NULL DEFAULT datetime(),
            UNIQUE(connection_id, owner, repo)
        );
-- table secret_github_binds
CREATE TABLE secret_github_binds (
            id TEXT PRIMARY KEY,
            secret_id TEXT NOT NULL REFERENCES secrets(id) ON DELETE CASCADE,
            repo_id TEXT NOT NULL REFERENCES github_repos(id) ON DELETE CASCADE,
            last_pushed_at TEXT,
            last_status TEXT NOT NULL DEFAULT 'pending',
            last_error TEXT NOT NULL DEFAULT '',
            last_op TEXT NOT NULL DEFAULT 'push',
            override_value TEXT,
            created_at TEXT NOT NULL DEFAULT datetime(),
            updated_at TEXT NOT NULL DEFAULT datetime(),
            UNIQUE(secret_id, repo_id)
        );
-- table variable_github_binds
CREATE TABLE variable_github_binds (
            id TEXT PRIMARY KEY,
            var_id TEXT NOT NULL REFERENCES platform_vars(id) ON DELETE CASCADE,
            repo_id TEXT NOT NULL REFERENCES github_repos(id) ON DELETE CASCADE,
            last_pushed_at TEXT,
            last_status TEXT NOT NULL DEFAULT 'pending',
            last_error TEXT NOT NULL DEFAULT '',
            last_op TEXT NOT NULL DEFAULT 'push',
            override_value TEXT,
            created_at TEXT NOT NULL DEFAULT datetime(),
            updated_at TEXT NOT NULL DEFAULT datetime(),
            UNIQUE(var_id, repo_id)
        );
-- table environments
CREATE TABLE environments (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            slug TEXT NOT NULL UNIQUE,
            description TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT datetime(),
            updated_at TEXT NOT NULL DEFAULT datetime()
        , protection_enabled BIGINT NOT NULL DEFAULT 0, min_trigger_role TEXT NOT NULL DEFAULT 'developer', min_approve_role TEXT NOT NULL DEFAULT 'maintainer', require_approvals BIGINT NOT NULL DEFAULT 1, forbid_self_approve BIGINT NOT NULL DEFAULT 1, approval_timeout_seconds BIGINT NOT NULL DEFAULT 86400, deployment_concurrency TEXT NOT NULL DEFAULT 'off');
-- table environment_var_overrides
CREATE TABLE environment_var_overrides (
            environment_id TEXT NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
            var_id TEXT NOT NULL REFERENCES platform_vars(id) ON DELETE CASCADE,
            value TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL DEFAULT datetime(),
            PRIMARY KEY (environment_id, var_id)
        );
-- table environment_secret_overrides
CREATE TABLE environment_secret_overrides (
            environment_id TEXT NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
            secret_id TEXT NOT NULL REFERENCES secrets(id) ON DELETE CASCADE,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT datetime(),
            PRIMARY KEY (environment_id, secret_id)
        );
-- table environment_hosts
CREATE TABLE environment_hosts (environment_id TEXT NOT NULL REFERENCES environments(id) ON DELETE CASCADE,kind TEXT NOT NULL CHECK (kind IN ('name', 'label')),value TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT datetime(),UNIQUE(environment_id, kind, value));
-- table runners
CREATE TABLE runners (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    register_token TEXT NOT NULL,
    token TEXT,
    labels TEXT NOT NULL DEFAULT '[]',
    team_id TEXT REFERENCES teams(id) ON DELETE RESTRICT,
    execution_mode TEXT NOT NULL DEFAULT 'host',
    status TEXT NOT NULL DEFAULT 'offline',
    version TEXT,
    system_info TEXT NOT NULL DEFAULT '{}',
    host_id TEXT NOT NULL DEFAULT '',
    runner_instance_id TEXT NOT NULL DEFAULT '',
    is_frozen BIGINT NOT NULL DEFAULT 0,
    restart_requested BIGINT NOT NULL DEFAULT 0,
    max_concurrent_jobs BIGINT NOT NULL DEFAULT 1,
    clean_workspace_after_job BIGINT NOT NULL DEFAULT 0,
    shell_enabled BIGINT NOT NULL DEFAULT 1,
    ssh_enabled BIGINT NOT NULL DEFAULT 1,
    docker_enabled BIGINT NOT NULL DEFAULT 0,
    wasm_enabled BIGINT NOT NULL DEFAULT 0,
    ssh_host_key_policy TEXT NOT NULL DEFAULT 'known_hosts',
    last_seen TEXT,
    created_at TEXT NOT NULL DEFAULT datetime(),
    updated_at TEXT NOT NULL DEFAULT datetime()
, claim_strategy TEXT NOT NULL DEFAULT 'backfill', sandbox_instance_id TEXT NOT NULL DEFAULT '', capability_reprobe_requested BIGINT NOT NULL DEFAULT 0, capability_reprobe_requested_at TEXT, capability_reprobe_served_at TEXT, netns_isolation BIGINT NOT NULL DEFAULT 0);
-- table jobs
CREATE TABLE jobs (
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
    retry_times BIGINT NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT datetime(),
    updated_at TEXT NOT NULL DEFAULT datetime()
, resource_profile TEXT NOT NULL DEFAULT '', resource_slots BIGINT NOT NULL DEFAULT 1, resource_spec TEXT NOT NULL DEFAULT '{}', sandbox_instance_id TEXT NOT NULL DEFAULT '', dispatch_reason TEXT NOT NULL DEFAULT '', logs_purged BIGINT NOT NULL DEFAULT 0, debug_unredact BIGINT NOT NULL DEFAULT 0, verbose_logs BIGINT NOT NULL DEFAULT 0, run_number BIGINT NOT NULL DEFAULT 0, run_attempt BIGINT NOT NULL DEFAULT 1, cancel_ack_pending BIGINT NOT NULL DEFAULT 0, truncated_steps TEXT NOT NULL DEFAULT '[]', queue_position BIGINT, environment_id TEXT REFERENCES environments(id) ON DELETE SET NULL, trigger_user_id TEXT, seq BIGINT NOT NULL DEFAULT 0);
-- table job_logs
CREATE TABLE job_logs (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    chunk_index BIGINT NOT NULL,
    step_order BIGINT NOT NULL DEFAULT 0,
    log_content TEXT NOT NULL,
    is_masked BIGINT NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT datetime(), level TEXT,
    UNIQUE(job_id, chunk_index)
);
-- table team_resource_grants
CREATE TABLE team_resource_grants (
    team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    profile TEXT NOT NULL,
    granted_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT datetime(),
    PRIMARY KEY (team_id, profile)
);
-- table pipeline_secrets
CREATE TABLE pipeline_secrets (
            pipeline_id TEXT NOT NULL REFERENCES pipelines(id) ON DELETE CASCADE,
            secret_id TEXT NOT NULL REFERENCES secrets(id) ON DELETE CASCADE,
            UNIQUE(pipeline_id, secret_id)
        );
-- table runner_events
CREATE TABLE runner_events (
            id TEXT PRIMARY KEY,
            runner_id TEXT NOT NULL REFERENCES runners(id) ON DELETE CASCADE,
            event_type TEXT NOT NULL,
            message TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT datetime()
        );
-- table job_approvals
CREATE TABLE job_approvals (
            id TEXT PRIMARY KEY,
            job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            user_id TEXT NOT NULL,
            username TEXT NOT NULL DEFAULT '',
            action TEXT NOT NULL,
            comment TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT datetime(),
            UNIQUE(job_id, user_id, action)
        );
-- ───────────────────────────── indexes ─────────────────────────────
-- index idx_api_tokens_user
CREATE INDEX idx_api_tokens_user ON api_tokens(user_id);

-- index idx_audit_logs_action
CREATE INDEX idx_audit_logs_action ON audit_logs(action);

-- index idx_audit_logs_actor
CREATE INDEX idx_audit_logs_actor ON audit_logs(actor_username);

-- index idx_audit_logs_created
CREATE INDEX idx_audit_logs_created ON audit_logs(created_at DESC);

-- index idx_audit_logs_resource
CREATE INDEX idx_audit_logs_resource
         ON audit_logs(resource_type, resource_id);

-- index idx_audit_logs_seq
CREATE INDEX idx_audit_logs_seq ON audit_logs(seq);

-- index idx_environment_secret_overrides_secret
CREATE INDEX idx_environment_secret_overrides_secret
         ON environment_secret_overrides(secret_id);

-- index idx_environment_var_overrides_var
CREATE INDEX idx_environment_var_overrides_var
         ON environment_var_overrides(var_id);

-- index idx_github_repos_connection
CREATE INDEX idx_github_repos_connection
         ON github_repos(connection_id);

-- index idx_hosts_team
CREATE INDEX idx_hosts_team ON hosts(team_id);

-- index idx_job_approvals_job
CREATE INDEX idx_job_approvals_job ON job_approvals(job_id);

-- index idx_job_logs_job
CREATE INDEX idx_job_logs_job ON job_logs(job_id, chunk_index);

-- index idx_jobs_environment
CREATE INDEX idx_jobs_environment ON jobs(environment_id);

-- index idx_jobs_pipeline
CREATE INDEX idx_jobs_pipeline ON jobs(pipeline_id);

-- index idx_jobs_pipeline_run_number
CREATE UNIQUE INDEX idx_jobs_pipeline_run_number ON jobs(pipeline_id, run_number, run_attempt) WHERE run_number > 0;

-- index idx_jobs_runner
CREATE INDEX idx_jobs_runner ON jobs(runner_id);

-- index idx_jobs_runner_cancel_ack
CREATE INDEX idx_jobs_runner_cancel_ack ON jobs(runner_id) WHERE cancel_ack_pending = 1;

-- index idx_jobs_seq
CREATE INDEX idx_jobs_seq ON jobs(seq);

-- index idx_jobs_status
CREATE INDEX idx_jobs_status ON jobs(status);

-- index idx_jobs_waiting_approval
CREATE INDEX idx_jobs_waiting_approval
         ON jobs(status) WHERE status = 'waiting_approval';

-- index idx_pipeline_git_hooks_key
CREATE INDEX idx_pipeline_git_hooks_key ON pipeline_git_hooks(hook_key);

-- index idx_pipeline_git_hooks_pipeline
CREATE INDEX idx_pipeline_git_hooks_pipeline ON pipeline_git_hooks(pipeline_id);

-- index idx_pipeline_secrets_pipeline
CREATE INDEX idx_pipeline_secrets_pipeline
         ON pipeline_secrets(pipeline_id);

-- index idx_pipeline_vars_pipeline
CREATE INDEX idx_pipeline_vars_pipeline
         ON pipeline_vars(pipeline_id);

-- index idx_pipelines_team
CREATE INDEX idx_pipelines_team ON pipelines(team_id);

-- index idx_pipelines_yaml_path
CREATE UNIQUE INDEX idx_pipelines_yaml_path
         ON pipelines(yaml_path) WHERE source = 'yaml';

-- index idx_plugin_pool_lookup
CREATE INDEX idx_plugin_pool_lookup ON plugin_pool (namespace, plugin_id, enabled);

-- index idx_runner_events_runner
CREATE INDEX idx_runner_events_runner
         ON runner_events(runner_id, created_at DESC);

-- index idx_runners_team
CREATE INDEX idx_runners_team ON runners(team_id);

-- index idx_sandbox_config_changes_status
CREATE INDEX idx_sandbox_config_changes_status
    ON sandbox_config_changes(status);

-- index idx_sandbox_hosts_host
CREATE INDEX idx_sandbox_hosts_host ON sandbox_hosts(host_id);

-- index idx_secret_github_binds_secret
CREATE INDEX idx_secret_github_binds_secret
         ON secret_github_binds(secret_id);

-- index idx_secrets_team
CREATE INDEX idx_secrets_team ON secrets(team_id);

-- index idx_step_resolutions_pipeline_order_v2
CREATE INDEX idx_step_resolutions_pipeline_order_v2 ON step_resolutions(pipeline_id, step_order);

-- index idx_tasks_pipeline
CREATE INDEX idx_tasks_pipeline ON tasks(pipeline_id, step_order);

-- index idx_team_members_user
CREATE INDEX idx_team_members_user ON team_members(user_id);

-- index idx_trigger_events_delivery
CREATE UNIQUE INDEX idx_trigger_events_delivery
         ON pipeline_trigger_events(pipeline_id, delivery_id)
         WHERE delivery_id IS NOT NULL AND delivery_id != '';

-- index idx_trigger_events_pipeline
CREATE INDEX idx_trigger_events_pipeline
         ON pipeline_trigger_events(pipeline_id, received_at DESC);

-- index idx_variable_github_binds_var
CREATE INDEX idx_variable_github_binds_var
         ON variable_github_binds(var_id);

-- index ux_step_resolutions_pipeline_order
CREATE UNIQUE INDEX ux_step_resolutions_pipeline_order ON step_resolutions(pipeline_id, step_order);

-- ───────────────────────────── triggers ─────────────────────────────
-- seq allocation (max+1 under the writer lock), ported from the SQLite trigger
CREATE OR REPLACE FUNCTION trg_jobs_insert_seq_fn() RETURNS trigger AS $$
BEGIN
    UPDATE jobs SET seq = (SELECT COALESCE(MAX(seq), 0) + 1 FROM jobs)
             WHERE id = NEW.id;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_jobs_insert_seq
    AFTER INSERT ON jobs
    FOR EACH ROW
    WHEN (NEW.seq = 0)
    EXECUTE FUNCTION trg_jobs_insert_seq_fn();

-- seq allocation (max+1 under the writer lock), ported from the SQLite trigger
CREATE OR REPLACE FUNCTION trg_audit_logs_insert_seq_fn() RETURNS trigger AS $$
BEGIN
    UPDATE audit_logs SET seq = (SELECT COALESCE(MAX(seq), 0) + 1 FROM audit_logs)
             WHERE id = NEW.id;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_audit_logs_insert_seq
    AFTER INSERT ON audit_logs
    FOR EACH ROW
    WHEN (NEW.seq = 0)
    EXECUTE FUNCTION trg_audit_logs_insert_seq_fn();

