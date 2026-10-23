use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};

use crate::config::Config;

pub async fn init(config: &Config) -> anyhow::Result<SqlitePool> {
    // Ensure data directory exists
    if let Some(parent) = std::path::Path::new(&config.database.path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&format!("sqlite:{}?mode=rwc", config.database.path))
        .await?;

    // ── Performance & reliability PRAGMAs ──

    // WAL mode: concurrent reads + one writer without blocking readers
    sqlx::query("PRAGMA journal_mode=WAL;")
        .execute(&pool)
        .await?;

    // NORMAL is safe with WAL; FULL would halve write throughput
    sqlx::query("PRAGMA synchronous=NORMAL;")
        .execute(&pool)
        .await?;

    sqlx::query("PRAGMA foreign_keys=ON;")
        .execute(&pool)
        .await?;

    // Wait up to 5 s when a write lock is held instead of immediately
    // returning SQLITE_BUSY.  This prevents transient contention between
    // runner heartbeat / poll / log-upload from failing requests.
    sqlx::query("PRAGMA busy_timeout=5000;")
        .execute(&pool)
        .await?;

    // Increase page‑cache for faster repeated reads (8 MB from 2 MB default)
    sqlx::query("PRAGMA cache_size=-8000;")
        .execute(&pool)
        .await?;

    // Memory‑mapped I/O for hot data (256 MB virtual map).  Safe on 64‑bit.
    sqlx::query("PRAGMA mmap_size=268435456;")
        .execute(&pool)
        .await?;

    // Auto‑checkpoint less aggressively to avoid write pauses
    sqlx::query("PRAGMA wal_autocheckpoint=1000;")
        .execute(&pool)
        .await?;

    tracing::info!(
        "Database initialized: {} (pool={}, WAL, busy_timeout=5s, cache=8MB, mmap=256MB)",
        config.database.path,
        config.database.max_connections
    );

    Ok(pool)
}

pub async fn run_migrations(pool: &SqlitePool, jwt_secret: &str) -> anyhow::Result<()> {
    for path in [
        include_str!("migrations/001_init.sql"),
        include_str!("migrations/002_step_resolutions.sql"),
    ] {
        // Split by ; and execute each statement
        for statement in path.split(';') {
            let trimmed = statement.trim();
            if !trimmed.is_empty() {
                sqlx::query(trimmed).execute(pool).await?;
            }
        }
    }

    // Add YAML content hash for file-backed pipeline projection locking.
    let _ = sqlx::query("ALTER TABLE pipelines ADD COLUMN yaml_sha256 TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // Add snapshot column to existing databases (safe to fail if already present)
    let _ = sqlx::query("ALTER TABLE jobs ADD COLUMN snapshot TEXT NOT NULL DEFAULT '{}'")
        .execute(pool)
        .await;

    // Add system_info column to runners table (safe to fail if already present)
    let _ = sqlx::query("ALTER TABLE runners ADD COLUMN system_info TEXT NOT NULL DEFAULT '{}'")
        .execute(pool)
        .await;

    // Add privileged column to tasks table (safe to fail if already present)
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN privileged INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;

    // Add input_defs column to pipelines table (safe to fail if already present)
    let _ = sqlx::query("ALTER TABLE pipelines ADD COLUMN input_defs TEXT NOT NULL DEFAULT '[]'")
        .execute(pool)
        .await;

    // Add level column to job_logs (chunk severity; NULL = legacy chunk)
    let _ = sqlx::query("ALTER TABLE job_logs ADD COLUMN level TEXT")
        .execute(pool)
        .await;

    migrate_step_resolutions(pool).await?;

    // Add webhook_secret column (bare team secret name; empty = no verification)
    let _ = sqlx::query("ALTER TABLE pipelines ADD COLUMN webhook_secret TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // Add is_frozen column to runners table (safe to fail if already present)
    let _ = sqlx::query("ALTER TABLE runners ADD COLUMN is_frozen INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;

    // One-shot restart flag: set by POST /runners/{id}/restart, served via the
    // next heartbeat, then cleared. Also blocks dispatch (like freeze) during
    // the request window so pending jobs don't sneak in before the restart lands.
    let _ =
        sqlx::query("ALTER TABLE runners ADD COLUMN restart_requested INTEGER NOT NULL DEFAULT 0")
            .execute(pool)
            .await;

    // Max parallel jobs per runner (UI-controlled; default 1 = serial)
    let _ = sqlx::query(
        "ALTER TABLE runners ADD COLUMN max_concurrent_jobs INTEGER NOT NULL DEFAULT 1",
    )
    .execute(pool)
    .await;

    // UI-managed runner policy (cleanup / engines / SSH host-key policy)
    let _ = sqlx::query(
        "ALTER TABLE runners ADD COLUMN clean_workspace_after_job INTEGER NOT NULL DEFAULT 0",
    )
    .execute(pool)
    .await;
    // Removed: runners.ephemeral (process-exit / redundant with docker --rm + clean_workspace)
    let _ = sqlx::query("ALTER TABLE runners DROP COLUMN ephemeral")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE runners ADD COLUMN shell_enabled INTEGER NOT NULL DEFAULT 1")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE runners ADD COLUMN ssh_enabled INTEGER NOT NULL DEFAULT 1")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE runners ADD COLUMN docker_enabled INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE runners ADD COLUMN wasm_enabled INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;
    let _ = sqlx::query(
        "ALTER TABLE runners ADD COLUMN ssh_host_key_policy TEXT NOT NULL DEFAULT 'known_hosts'",
    )
    .execute(pool)
    .await;

    // Add host_labels column to tasks table (safe to fail if already present)
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN host_labels TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // Docker engine: container image for uses=docker steps
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN image TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // Docker engine: extra host→container volume mounts (JSON array or comma-separated)
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN volumes TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    let task_columns: Vec<String> = sqlx::query("PRAGMA table_info(tasks)")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|row| row.get::<String, _>(1))
        .collect();
    if !task_columns.iter().any(|column| column == "capabilities") {
        sqlx::query("ALTER TABLE tasks ADD COLUMN capabilities TEXT NOT NULL DEFAULT '[]'")
            .execute(pool)
            .await?;
    }

    // Add step_statuses column to jobs table (safe to fail if already present)
    let _ = sqlx::query("ALTER TABLE jobs ADD COLUMN step_statuses TEXT NOT NULL DEFAULT '[]'")
        .execute(pool)
        .await;

    // Admin-only debug flag: when set, the Runner skips log redaction for this job.
    // Dispatched to the Runner and used to gate debug-job log reads to admins.
    let _ = sqlx::query("ALTER TABLE jobs ADD COLUMN debug_unredact INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;

    // Verbose execution mode: any user can trigger with extra detail in the
    // Runner's logs (per-step duration, exit code, stage markers). Redaction
    // stays fully intact — distinct from the admin-only debug_unredact flag.
    let _ = sqlx::query("ALTER TABLE jobs ADD COLUMN verbose_logs INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;

    // Add owner_group column to secrets table (safe to fail if already present)
    let _ = sqlx::query("ALTER TABLE secrets ADD COLUMN owner_group TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // Add owner_group column to pipelines table (safe to fail if already present)
    let _ = sqlx::query("ALTER TABLE pipelines ADD COLUMN owner_group TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // ── Multi-env governance: mandatory pipeline environments ──
    let _ = sqlx::query("ALTER TABLE pipelines ADD COLUMN environments TEXT NOT NULL DEFAULT '[]'")
        .execute(pool)
        .await;
    let _ = sqlx::query(
        "ALTER TABLE pipelines ADD COLUMN webhook_environment TEXT NOT NULL DEFAULT ''",
    )
    .execute(pool)
    .await;
    let _ =
        sqlx::query("ALTER TABLE pipelines ADD COLUMN cron_environment TEXT NOT NULL DEFAULT ''")
            .execute(pool)
            .await;
    // Backfill: unbound pipelines target the default environment (created
    // below, after the environments table exists).
    sqlx::query("UPDATE pipelines SET environments = '[\"default\"]' WHERE environments = '[]'")
        .execute(pool)
        .await?;

    // GHA-style per-pipeline build numbers. `pipelines.run_counter` is the
    // allocation cursor (atomic UPDATE … RETURNING at trigger time);
    // `jobs.run_number` is the immutable number a job consumed; `run_attempt`
    // is 1 until a job re-run feature exists. Legacy jobs are backfilled by
    // created_at order below so counters continue from history.
    let _ = sqlx::query("ALTER TABLE pipelines ADD COLUMN run_counter INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE jobs ADD COLUMN run_number INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE jobs ADD COLUMN run_attempt INTEGER NOT NULL DEFAULT 1")
        .execute(pool)
        .await;
    backfill_job_run_numbers(pool).await?;
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_pipeline_run_number \
         ON jobs(pipeline_id, run_number) WHERE run_number > 0",
    )
    .execute(pool)
    .await?;

    // ── Cancel delivery over the claim channel ──
    // `cancel_ack_pending` is 1 from the moment a job flips to `cancelled`
    // (operator cancel, pipeline disable, cancel_in_progress supersede)
    // until the owning runner's next claim poll reports the id — one-shot
    // delivery flag replacing the runner's per-job 2s status-check probes.
    let _ =
        sqlx::query("ALTER TABLE jobs ADD COLUMN cancel_ack_pending INTEGER NOT NULL DEFAULT 0")
            .execute(pool)
            .await;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_jobs_runner_cancel_ack \
         ON jobs(runner_id) WHERE cancel_ack_pending = 1",
    )
    .execute(pool)
    .await?;

    // ── Pipeline ↔ Secret explicit binding (many-to-many) ──
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS pipeline_secrets (
            pipeline_id TEXT NOT NULL REFERENCES pipelines(id) ON DELETE CASCADE,
            secret_id TEXT NOT NULL REFERENCES secrets(id) ON DELETE CASCADE,
            UNIQUE(pipeline_id, secret_id)
        )",
    )
    .execute(pool)
    .await?;

    // Index for fast lookup by pipeline_id
    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pipeline_secrets_pipeline
         ON pipeline_secrets(pipeline_id)",
    )
    .execute(pool)
    .await;

    // ── Infrastructure tables (idempotent CREATE TABLE) ──
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS secrets (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            value TEXT NOT NULL DEFAULT '',
            owner_group TEXT NOT NULL DEFAULT '',
            team_shared INTEGER NOT NULL DEFAULT 0,
            kind TEXT NOT NULL DEFAULT 'generic',
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS platform_vars (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            value TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS runner_events (
            id TEXT PRIMARY KEY,
            runner_id TEXT NOT NULL REFERENCES runners(id) ON DELETE CASCADE,
            event_type TEXT NOT NULL,
            message TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;
    // Index for fast lookups per runner, newest-first
    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_runner_events_runner
         ON runner_events(runner_id, created_at DESC)",
    )
    .execute(pool)
    .await;

    // ── Pipeline ↔ Variable binding (many-to-many) ──
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS pipeline_vars (
            pipeline_id TEXT NOT NULL REFERENCES pipelines(id) ON DELETE CASCADE,
            var_id TEXT NOT NULL REFERENCES platform_vars(id) ON DELETE CASCADE,
            UNIQUE(pipeline_id, var_id)
        )",
    )
    .execute(pool)
    .await?;

    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pipeline_vars_pipeline
         ON pipeline_vars(pipeline_id)",
    )
    .execute(pool)
    .await;

    // Add value column to variables table for inline env vars (safe to fail if already present)
    let _ = sqlx::query("ALTER TABLE variables ADD COLUMN value TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // Add host_names column to tasks — stores original host names before ID resolution
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN host_names TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // Add step_secrets column to tasks — per-step secret allowlist
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN step_secrets TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // Step-level env map JSON (`{"KEY":"${secrets.NAME}"}`) — GHA-style mapping
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN step_env TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // Private registry credentials secret name (docker / shell / SSH)
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN registry_secret TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // OSS config delivery + render (Config Block DSL v2 column names).
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN config_source TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;
    // Legacy columns (pre-2b) — may still exist on upgraded DBs until cut-over below.
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN config_path TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN env_render TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN config_dir TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // Phase 2b D2: rename to config_key_path / config_dest / config_render (one-shot).
    migrate_tasks_config_block_columns(pool).await?;

    // ── Secret name/env_name merge ──
    // Design decision: the secret `name` IS the injected env key and the
    // `${secrets.X}` template reference — there is no separate `env_name`.
    // New secrets must be UPPER_SNAKE_CASE (enforced at create). For legacy
    // rows that had a distinct env_name, backfill it into `name` (only when it
    // does not collide with another secret's name), then drop the column.
    //
    // Colliding rows keep their old `name` and permanently lose `env_name`
    // when the column is dropped — warn so operators can rename manually.
    {
        let has_env_name: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('secrets') WHERE name = 'env_name'",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(false);

        if has_env_name {
            let collisions = sqlx::query_as::<_, (String, String, String)>(
                "SELECT secrets.id, secrets.name, secrets.env_name
                 FROM secrets
                 WHERE env_name != '' AND env_name != name
                   AND EXISTS (
                       SELECT 1 FROM secrets s2
                       WHERE s2.name = secrets.env_name AND s2.id != secrets.id
                   )",
            )
            .fetch_all(pool)
            .await
            .unwrap_or_default();

            for (id, name, env_name) in &collisions {
                tracing::warn!(
                    secret_id = %id,
                    old_name = %name,
                    env_name = %env_name,
                    "secret env_name→name merge skipped: env_name already used by another secret; \
                     rename manually (Update API cannot change name — delete + recreate), \
                     then update pipeline refs to the UPPER_SNAKE_CASE name"
                );
            }

            match sqlx::query(
                "UPDATE secrets
                 SET name = env_name
                 WHERE env_name != '' AND env_name != name
                   AND NOT EXISTS (
                       SELECT 1 FROM secrets s2
                       WHERE s2.name = secrets.env_name AND s2.id != secrets.id
                   )",
            )
            .execute(pool)
            .await
            {
                Ok(r) => {
                    if r.rows_affected() > 0 {
                        tracing::info!(
                            rows = r.rows_affected(),
                            "secret env_name→name merge: backfilled name from env_name"
                        );
                    }
                }
                Err(e) => tracing::warn!("secret env_name→name merge UPDATE failed: {e}"),
            }

            if let Err(e) = sqlx::query("ALTER TABLE secrets DROP COLUMN env_name")
                .execute(pool)
                .await
            {
                tracing::warn!("secret env_name DROP COLUMN failed (may already be gone): {e}");
            } else {
                tracing::info!("secret env_name column dropped");
            }
        }
    }

    // Add credential_data column to hosts — stores key content or password in DB
    let _ = sqlx::query("ALTER TABLE hosts ADD COLUMN credential_data TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // Drop legacy credential_ref column (safe to fail if already dropped or not supported)
    let _ = sqlx::query("ALTER TABLE hosts DROP COLUMN credential_ref")
        .execute(pool)
        .await;

    // Add credential_passphrase column to hosts — encrypted passphrase for key-based auth
    let _ =
        sqlx::query("ALTER TABLE hosts ADD COLUMN credential_passphrase TEXT NOT NULL DEFAULT ''")
            .execute(pool)
            .await;

    // ── Phase 2 RBAC: users / teams / memberships ──
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            display_name TEXT NOT NULL DEFAULT '',
            is_platform_admin INTEGER NOT NULL DEFAULT 0,
            is_active INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS teams (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE,
            description TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS team_members (
            team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
            user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            role TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (team_id, user_id)
        )",
    )
    .execute(pool)
    .await?;

    let _ =
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_team_members_user ON team_members(user_id)")
            .execute(pool)
            .await;

    // Resource team_id columns (empty string until bootstrap backfill)
    let _ = sqlx::query("ALTER TABLE pipelines ADD COLUMN team_id TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    let _ = sqlx::query("ALTER TABLE secrets ADD COLUMN team_id TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // Opt-in team auto-inject (default false = explicit bind/reference only)
    let _ = sqlx::query("ALTER TABLE secrets ADD COLUMN team_shared INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;

    // Secret purpose: generic (env) | registry (docker login JSON)
    let _ = sqlx::query("ALTER TABLE secrets ADD COLUMN kind TEXT NOT NULL DEFAULT 'generic'")
        .execute(pool)
        .await;

    let _ = sqlx::query("ALTER TABLE hosts ADD COLUMN team_id TEXT")
        .execute(pool)
        .await;

    // Runner team binding: NULL remains the explicit shared/legacy mode.
    let _ = sqlx::query(
        "ALTER TABLE runners ADD COLUMN team_id TEXT REFERENCES teams(id) ON DELETE RESTRICT",
    )
    .execute(pool)
    .await;
    let _ =
        sqlx::query("ALTER TABLE runners ADD COLUMN execution_mode TEXT NOT NULL DEFAULT 'host'")
            .execute(pool)
            .await;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_runners_team ON runners(team_id)")
        .execute(pool)
        .await?;

    let _ = sqlx::query("CREATE INDEX IF NOT EXISTS idx_pipelines_team ON pipelines(team_id)")
        .execute(pool)
        .await;
    let _ = sqlx::query("CREATE INDEX IF NOT EXISTS idx_secrets_team ON secrets(team_id)")
        .execute(pool)
        .await;
    let _ = sqlx::query("CREATE INDEX IF NOT EXISTS idx_hosts_team ON hosts(team_id)")
        .execute(pool)
        .await;

    // ── Concurrency queue mode (Phase 7) ──
    let _ =
        sqlx::query("ALTER TABLE pipelines ADD COLUMN concurrency_mode TEXT NOT NULL DEFAULT ''")
            .execute(pool)
            .await;

    let _ = sqlx::query(
        "ALTER TABLE pipelines ADD COLUMN concurrency_max_queue INTEGER NOT NULL DEFAULT 3",
    )
    .execute(pool)
    .await;

    let _ = sqlx::query("ALTER TABLE pipelines ADD COLUMN concurrency_queue_timeout_seconds INTEGER NOT NULL DEFAULT 1800")
        .execute(pool)
        .await;

    let _ = sqlx::query("ALTER TABLE jobs ADD COLUMN queue_position INTEGER")
        .execute(pool)
        .await;

    // ── Platform settings (admin-editable runtime overrides) ──
    // DB value > manager.toml default; a missing row means "use the default".
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS platform_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_by TEXT,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    // ── Scheduler leader-lease table (single-row singleton) ──
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS scheduler_lease (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            instance_id TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    // ── Git webhook hooks + unified trigger events ──
    // Recover / upgrade first (may rename leftover `pipeline_git_hooks_new`).
    migrate_pipeline_git_hooks_multi(pool).await?;

    // Multiple hooks per pipeline allowed (pipeline_id is not UNIQUE).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS pipeline_git_hooks (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            label TEXT NOT NULL DEFAULT '',
            provider TEXT NOT NULL,
            hook_key TEXT NOT NULL UNIQUE,
            secret_ciphertext TEXT NOT NULL,
            branch_filter TEXT NOT NULL DEFAULT '',
            environment TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY (pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        )",
    )
    .execute(pool)
    .await?;

    let _ = sqlx::query("ALTER TABLE pipeline_git_hooks ADD COLUMN label TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;
    let _ = sqlx::query(
        "ALTER TABLE pipeline_git_hooks ADD COLUMN environment TEXT NOT NULL DEFAULT ''",
    )
    .execute(pool)
    .await;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pipeline_git_hooks_key ON pipeline_git_hooks(hook_key)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pipeline_git_hooks_pipeline ON pipeline_git_hooks(pipeline_id)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS pipeline_trigger_events (
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
            received_at TEXT NOT NULL DEFAULT (datetime('now')),
            decision TEXT NOT NULL,
            skip_reason TEXT NOT NULL DEFAULT '',
            job_id TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY (pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_trigger_events_pipeline
         ON pipeline_trigger_events(pipeline_id, received_at DESC)",
    )
    .execute(pool)
    .await?;

    // Partial uniqueness: ignore NULL delivery_id (manual/cron)
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_trigger_events_delivery
         ON pipeline_trigger_events(pipeline_id, delivery_id)
         WHERE delivery_id IS NOT NULL AND delivery_id != ''",
    )
    .execute(pool)
    .await?;

    // ── Admin operation audit log ──
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS audit_logs (
            id TEXT PRIMARY KEY,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
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
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_audit_logs_created ON audit_logs(created_at DESC)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_audit_logs_action ON audit_logs(action)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_audit_logs_actor ON audit_logs(actor_username)")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_audit_logs_resource
         ON audit_logs(resource_type, resource_id)",
    )
    .execute(pool)
    .await?;

    // ── Phase 3: platform-admin tiers + API tokens ──
    // Super admin sees all audit logs and all API tokens; regular platform
    // admins see only their own. Safe to fail when the column already exists.
    let _ = sqlx::query("ALTER TABLE users ADD COLUMN is_super_admin INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS api_tokens (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            name TEXT NOT NULL,
            token_hash TEXT NOT NULL,
            token_prefix TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            last_used_at TEXT,
            revoked_at TEXT,
            FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_api_tokens_user ON api_tokens(user_id)")
        .execute(pool)
        .await?;

    // ── GitHub Actions sync (connections / repos / secret & var binds) ──
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS github_connections (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            api_base_url TEXT NOT NULL DEFAULT 'https://api.github.com',
            token_ciphertext TEXT NOT NULL,
            fetch_owners TEXT NOT NULL DEFAULT '',
            fetch_interval_minutes INTEGER,
            last_fetch_at TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;
    // Migrate pre-existing connections (v1 shipped without auto-fetch columns).
    let _ = sqlx::query(
        "ALTER TABLE github_connections ADD COLUMN fetch_owners TEXT NOT NULL DEFAULT ''",
    )
    .execute(pool)
    .await;
    let _ = sqlx::query("ALTER TABLE github_connections ADD COLUMN fetch_interval_minutes INTEGER")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE github_connections ADD COLUMN last_fetch_at TEXT")
        .execute(pool)
        .await;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS github_repos (
            id TEXT PRIMARY KEY,
            connection_id TEXT NOT NULL REFERENCES github_connections(id) ON DELETE CASCADE,
            owner TEXT NOT NULL,
            repo TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(connection_id, owner, repo)
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS secret_github_binds (
            id TEXT PRIMARY KEY,
            secret_id TEXT NOT NULL REFERENCES secrets(id) ON DELETE CASCADE,
            repo_id TEXT NOT NULL REFERENCES github_repos(id) ON DELETE CASCADE,
            last_pushed_at TEXT,
            last_status TEXT NOT NULL DEFAULT 'pending',
            last_error TEXT NOT NULL DEFAULT '',
            last_op TEXT NOT NULL DEFAULT 'push',
            override_value TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(secret_id, repo_id)
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS variable_github_binds (
            id TEXT PRIMARY KEY,
            var_id TEXT NOT NULL REFERENCES platform_vars(id) ON DELETE CASCADE,
            repo_id TEXT NOT NULL REFERENCES github_repos(id) ON DELETE CASCADE,
            last_pushed_at TEXT,
            last_status TEXT NOT NULL DEFAULT 'pending',
            last_error TEXT NOT NULL DEFAULT '',
            last_op TEXT NOT NULL DEFAULT 'push',
            override_value TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(var_id, repo_id)
        )",
    )
    .execute(pool)
    .await?;
    // Migrate pre-existing bind rows (v1 shipped without a last-op marker).
    let _ = sqlx::query(
        "ALTER TABLE secret_github_binds ADD COLUMN last_op TEXT NOT NULL DEFAULT 'push'",
    )
    .execute(pool)
    .await;
    let _ = sqlx::query(
        "ALTER TABLE variable_github_binds ADD COLUMN last_op TEXT NOT NULL DEFAULT 'push'",
    )
    .execute(pool)
    .await;
    // Per-repo push value: variables store plaintext; secrets store AES ciphertext.
    // NULL = inherit platform_vars.value / secrets.value.
    let _ = sqlx::query("ALTER TABLE variable_github_binds ADD COLUMN override_value TEXT")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE secret_github_binds ADD COLUMN override_value TEXT")
        .execute(pool)
        .await;

    // ── Platform Environments (Variable + Secret per-env overrides) ──
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS environments (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            slug TEXT NOT NULL UNIQUE,
            description TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS environment_var_overrides (
            environment_id TEXT NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
            var_id TEXT NOT NULL REFERENCES platform_vars(id) ON DELETE CASCADE,
            value TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (environment_id, var_id)
        )",
    )
    .execute(pool)
    .await?;

    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_environment_var_overrides_var
         ON environment_var_overrides(var_id)",
    )
    .execute(pool)
    .await;

    // Secret overrides: AES ciphertext (same scheme as secrets.value). Never return plaintext.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS environment_secret_overrides (
            environment_id TEXT NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
            secret_id TEXT NOT NULL REFERENCES secrets(id) ON DELETE CASCADE,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (environment_id, secret_id)
        )",
    )
    .execute(pool)
    .await?;

    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_environment_secret_overrides_secret
         ON environment_secret_overrides(secret_id)",
    )
    .execute(pool)
    .await;

    // Job may optionally run under a platform Environment (NULL = defaults only).
    let _ = sqlx::query("ALTER TABLE jobs ADD COLUMN environment_id TEXT REFERENCES environments(id) ON DELETE SET NULL")
        .execute(pool)
        .await;

    let _ = sqlx::query("CREATE INDEX IF NOT EXISTS idx_jobs_environment ON jobs(environment_id)")
        .execute(pool)
        .await;

    // Trigger user id for self-approve checks (NULL for webhook/cron/git).
    let _ = sqlx::query("ALTER TABLE jobs ADD COLUMN trigger_user_id TEXT")
        .execute(pool)
        .await;

    // Environment protection / approval gates (方案 B).
    let _ = sqlx::query(
        "ALTER TABLE environments ADD COLUMN protection_enabled INTEGER NOT NULL DEFAULT 0",
    )
    .execute(pool)
    .await;
    let _ = sqlx::query(
        "ALTER TABLE environments ADD COLUMN min_trigger_role TEXT NOT NULL DEFAULT 'developer'",
    )
    .execute(pool)
    .await;
    let _ = sqlx::query(
        "ALTER TABLE environments ADD COLUMN min_approve_role TEXT NOT NULL DEFAULT 'maintainer'",
    )
    .execute(pool)
    .await;
    let _ = sqlx::query(
        "ALTER TABLE environments ADD COLUMN require_approvals INTEGER NOT NULL DEFAULT 1",
    )
    .execute(pool)
    .await;
    let _ = sqlx::query(
        "ALTER TABLE environments ADD COLUMN forbid_self_approve INTEGER NOT NULL DEFAULT 1",
    )
    .execute(pool)
    .await;
    let _ = sqlx::query(
        "ALTER TABLE environments ADD COLUMN approval_timeout_seconds INTEGER NOT NULL DEFAULT 86400",
    )
    .execute(pool)
    .await;

    // Deployment concurrency per environment (web-managed): off (default,
    // legacy behavior) | queue (serialize deployments) | cancel_in_progress.
    let _ = sqlx::query(
        "ALTER TABLE environments ADD COLUMN deployment_concurrency TEXT NOT NULL DEFAULT 'off'",
    )
    .execute(pool)
    .await;

    // Per-environment host allowlist: names or host labels a deployment to
    // this environment may touch (empty table = unrestricted).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS environment_hosts (\
            environment_id TEXT NOT NULL REFERENCES environments(id) ON DELETE CASCADE,\
            kind TEXT NOT NULL CHECK (kind IN ('name', 'label')),\
            value TEXT NOT NULL,\
            created_at TEXT NOT NULL DEFAULT (datetime('now')),\
            UNIQUE(environment_id, kind, value)\
        )",
    )
    .execute(pool)
    .await?;
    let _ = sqlx::query(
        "ALTER TABLE environment_hosts ADD COLUMN created_at TEXT NOT NULL DEFAULT (datetime('now'))",
    )
    .execute(pool)
    .await;

    // A `default` environment (mirroring the `default` team) receives every
    // pipeline that predates explicit bindings, so protection can gate them.
    sqlx::query(
        "INSERT INTO environments (id, slug, name, description) \
         SELECT 'env-default', 'default', 'Default', \
                'Auto-created home for pipelines without an explicit environment' \
         WHERE NOT EXISTS (SELECT 1 FROM environments WHERE slug = 'default')",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS job_approvals (
            id TEXT PRIMARY KEY,
            job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            user_id TEXT NOT NULL,
            username TEXT NOT NULL DEFAULT '',
            action TEXT NOT NULL,
            comment TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(job_id, user_id, action)
        )",
    )
    .execute(pool)
    .await?;

    // Durable display name even if the user row is later removed.
    let _ = sqlx::query("ALTER TABLE job_approvals ADD COLUMN username TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    let _ =
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_job_approvals_job ON job_approvals(job_id)")
            .execute(pool)
            .await;

    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_jobs_waiting_approval
         ON jobs(status) WHERE status = 'waiting_approval'",
    )
    .execute(pool)
    .await;

    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_github_repos_connection
         ON github_repos(connection_id)",
    )
    .execute(pool)
    .await;
    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_secret_github_binds_secret
         ON secret_github_binds(secret_id)",
    )
    .execute(pool)
    .await;
    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_variable_github_binds_var
         ON variable_github_binds(var_id)",
    )
    .execute(pool)
    .await;

    backfill_registry_secret_kind(pool, jwt_secret).await?;

    tracing::info!("Database migrations applied successfully");
    Ok(())
}

async fn migrate_step_resolutions(pool: &SqlitePool) -> anyhow::Result<()> {
    for (column, definition) in [
        ("reviewed", "reviewed INTEGER NOT NULL DEFAULT 0"),
        ("step_order", "step_order INTEGER NOT NULL DEFAULT 0"),
        (
            "input_defs_json",
            "input_defs_json TEXT NOT NULL DEFAULT '[]'",
        ),
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('step_resolutions') WHERE name = ?",
        )
        .bind(column)
        .fetch_one(pool)
        .await?;
        if !exists {
            sqlx::query(&format!(
                "ALTER TABLE step_resolutions ADD COLUMN {definition}"
            ))
            .execute(pool)
            .await?;
        }
    }

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_step_resolutions_pipeline_order_v2 \
         ON step_resolutions(pipeline_id, step_order)",
    )
    .execute(pool)
    .await?;

    let duplicates: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM step_resolutions
            GROUP BY pipeline_id, step_order
            HAVING COUNT(*) > 1
        )",
    )
    .fetch_one(pool)
    .await?;
    if !duplicates {
        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS ux_step_resolutions_pipeline_order \
             ON step_resolutions(pipeline_id, step_order)",
        )
        .execute(pool)
        .await?;
    } else {
        tracing::warn!(
            "legacy step resolution rows contain duplicate pipeline/order pairs; \
             preserving them and deferring the unique index until pipelines are resaved"
        );
    }
    Ok(())
}

#[cfg(test)]
mod run_number_tests {
    use sqlx::Row;
    use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

    async fn pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    async fn seed_pipeline(pool: &SqlitePool, id: &str) {
        sqlx::query(
            "INSERT INTO pipelines (id, name, team_id, yaml_sha256) \
             VALUES (?, ?, 'default', '')",
        )
        .bind(id)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_legacy_job(pool: &SqlitePool, id: &str, pipeline_id: &str, created_at: &str) {
        sqlx::query(
            "INSERT INTO jobs (id, pipeline_id, status, trigger_by, created_at) \
             VALUES (?, ?, 'success', 'manual', ?)",
        )
        .bind(id)
        .bind(pipeline_id)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn backfill_numbers_legacy_jobs_and_advances_counters() {
        let pool = pool().await;
        crate::db::run_migrations(&pool, "test-secret-0123456789abcdef")
            .await
            .unwrap();
        seed_pipeline(&pool, "p1").await;
        // Inserted newest-first on purpose: backfill must order by created_at.
        seed_legacy_job(&pool, "j-new", "p1", "2026-01-03 00:00:00").await;
        seed_legacy_job(&pool, "j-mid", "p1", "2026-01-02 00:00:00").await;
        seed_legacy_job(&pool, "j-old", "p1", "2026-01-01 00:00:00").await;
        // Simulate the upgrade moment: these jobs existed before the feature.
        super::backfill_job_run_numbers(&pool).await.unwrap();

        async fn number(pool: &SqlitePool, job: &str) -> i64 {
            sqlx::query_scalar("SELECT run_number FROM jobs WHERE id = ?")
                .bind(job)
                .fetch_one(pool)
                .await
                .unwrap()
        }
        assert_eq!(number(&pool, "j-old").await, 1);
        assert_eq!(number(&pool, "j-mid").await, 2);
        assert_eq!(number(&pool, "j-new").await, 3);
        let counter: i64 = sqlx::query_scalar("SELECT run_counter FROM pipelines WHERE id = 'p1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(counter, 3);

        // Allocation continues after history.
        let mut conn = pool.acquire().await.unwrap();
        assert_eq!(
            crate::db::allocate_run_number(&mut conn, "p1")
                .await
                .unwrap(),
            4
        );
    }

    #[tokio::test]
    async fn concurrent_allocations_are_unique_and_dense() {
        let pool = pool().await;
        crate::db::run_migrations(&pool, "test-secret-0123456789abcdef")
            .await
            .unwrap();
        seed_pipeline(&pool, "p1").await;
        seed_pipeline(&pool, "p2").await;

        let mut handles = Vec::new();
        for _ in 0..16 {
            let pool = pool.clone();
            handles.push(tokio::spawn(async move {
                let conn = pool.acquire().await.unwrap();
                let mut conn = conn;
                crate::db::allocate_run_number(&mut conn, "p1")
                    .await
                    .unwrap()
            }));
        }
        let mut numbers = Vec::new();
        for handle in handles {
            numbers.push(handle.await.unwrap());
        }
        numbers.sort();
        let expected: Vec<i64> = (1..=16).collect();
        assert_eq!(numbers, expected, "gapless, unique 1..=16 under contention");

        // A different pipeline counts independently.
        let conn = pool.acquire().await.unwrap();
        let mut conn = conn;
        assert_eq!(
            crate::db::allocate_run_number(&mut conn, "p2")
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn partial_unique_index_rejects_duplicate_numbers() {
        let pool = pool().await;
        crate::db::run_migrations(&pool, "test-secret-0123456789abcdef")
            .await
            .unwrap();
        seed_pipeline(&pool, "p1").await;
        seed_legacy_job(&pool, "j1", "p1", "2026-01-01 00:00:00").await;
        super::backfill_job_run_numbers(&pool).await.unwrap();
        let first: i64 = sqlx::query_scalar("SELECT run_number FROM jobs WHERE id = 'j1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let dup = sqlx::query(
            "INSERT INTO jobs (id, pipeline_id, status, trigger_by, run_number) \
             VALUES ('j2', 'p1', 'pending', 'manual', ?)",
        )
        .bind(first)
        .execute(&pool)
        .await;
        assert!(
            dup.is_err(),
            "unique index must reject duplicate run numbers"
        );
        let _ = sqlx::query("SELECT 1")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get::<i64, _>(0);
    }
}

#[cfg(test)]
mod step_resolution_migration_tests {
    use super::migrate_step_resolutions;
    use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

    async fn pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn upgrades_legacy_table_without_losing_duplicate_rows() {
        let pool = pool().await;
        sqlx::query(
            "CREATE TABLE step_resolutions (
                id TEXT PRIMARY KEY,
                pipeline_id TEXT NOT NULL,
                uses_ref TEXT NOT NULL,
                resolved_version TEXT NOT NULL,
                content_sha256 TEXT NOT NULL,
                source TEXT NOT NULL,
                expanded_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO step_resolutions
             (id, pipeline_id, uses_ref, resolved_version, content_sha256, source, expanded_json)
             VALUES ('a', 'pipe', 'buz/x', '1.0.0', 'hash-a', 'official', '{}'),
                    ('b', 'pipe', 'buz/z', '1.0.0', 'hash-b', 'official', '{}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE INDEX idx_step_resolutions_pipeline_order \
             ON step_resolutions(pipeline_id)",
        )
        .execute(&pool)
        .await
        .unwrap();

        migrate_step_resolutions(&pool).await.unwrap();

        let row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM step_resolutions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row_count, 2);
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('step_resolutions')")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(columns.iter().any(|column| column == "step_order"));
        assert!(columns.iter().any(|column| column == "reviewed"));
        let unique_index: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM pragma_index_list('step_resolutions')
               WHERE name = 'ux_step_resolutions_pipeline_order' AND "unique" = 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(unique_index, 0);
    }

    #[tokio::test]
    async fn creates_unique_index_when_existing_orders_are_unambiguous() {
        let pool = pool().await;
        sqlx::query(
            "CREATE TABLE step_resolutions (
                id TEXT PRIMARY KEY,
                pipeline_id TEXT NOT NULL,
                step_order INTEGER NOT NULL DEFAULT 0,
                reviewed INTEGER NOT NULL DEFAULT 0,
                uses_ref TEXT NOT NULL,
                resolved_version TEXT NOT NULL,
                content_sha256 TEXT NOT NULL,
                source TEXT NOT NULL,
                expanded_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        migrate_step_resolutions(&pool).await.unwrap();
        let unique_index: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM pragma_index_list('step_resolutions')
               WHERE name = 'ux_step_resolutions_pipeline_order' AND "unique" = 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(unique_index, 1);
        let duplicate = sqlx::query(
            "INSERT INTO step_resolutions
             (id, pipeline_id, step_order, uses_ref, resolved_version, content_sha256, source, expanded_json)
             VALUES ('first', 'pipe', 0, 'buz/x', '1.0.0', 'a', 'official', '{}'),
                    ('second', 'pipe', 0, 'buz/z', '1.0.0', 'b', 'official', '{}')",
        )
        .execute(&pool)
        .await;
        assert!(duplicate.is_err());
    }
}

#[cfg(test)]
mod pipeline_git_hooks_migration_tests {
    use super::migrate_pipeline_git_hooks_multi;
    use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

    async fn pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    /// Minimal pipelines table so the `pipeline_git_hooks` foreign key and the
    /// rebuild INSERT…SELECT resolve (sqlx enables foreign_keys by default).
    async fn seed_pipelines(pool: &SqlitePool, ids: &[&str]) {
        sqlx::query("CREATE TABLE pipelines (id TEXT PRIMARY KEY)")
            .execute(pool)
            .await
            .unwrap();
        for id in ids {
            sqlx::query("INSERT INTO pipelines (id) VALUES (?)")
                .bind(id)
                .execute(pool)
                .await
                .unwrap();
        }
    }

    /// Pre-multi-hook schema: one hook per pipeline (`pipeline_id … UNIQUE`),
    /// no `label` and no `environment` column at all.
    async fn seed_oldest_schema(pool: &SqlitePool) {
        seed_pipelines(pool, &["pipe-a"]).await;
        sqlx::query(
            "CREATE TABLE pipeline_git_hooks (
                id TEXT PRIMARY KEY,
                pipeline_id TEXT NOT NULL UNIQUE,
                provider TEXT NOT NULL,
                hook_key TEXT NOT NULL UNIQUE,
                secret_ciphertext TEXT NOT NULL,
                branch_filter TEXT NOT NULL DEFAULT '',
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
            )",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO pipeline_git_hooks
             (id, pipeline_id, provider, hook_key, secret_ciphertext, branch_filter, enabled)
             VALUES ('hook-1', 'pipe-a', 'gitee', 'key-1', 'cipher-1', 'main', 1)",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn oldest_schema_gains_label_and_environment_columns() {
        let pool = pool().await;
        seed_oldest_schema(&pool).await;

        migrate_pipeline_git_hooks_multi(&pool).await.unwrap();

        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('pipeline_git_hooks')")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(columns.iter().any(|column| column == "label"));
        assert!(columns.iter().any(|column| column == "environment"));

        // Row survived the rebuild with the new columns defaulted.
        let row: (String, String, String, String, String, String, i64) = sqlx::query_as(
            "SELECT id, pipeline_id, provider, hook_key, branch_filter, environment, enabled \
             FROM pipeline_git_hooks WHERE id = 'hook-1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "hook-1");
        assert_eq!(row.1, "pipe-a");
        assert_eq!(row.2, "gitee");
        assert_eq!(row.3, "key-1");
        assert_eq!(row.4, "main");
        assert_eq!(row.5, "", "legacy hook has no environment binding");
        assert_eq!(row.6, 1);

        // The one-hook-per-pipeline UNIQUE constraint is gone.
        let second = sqlx::query(
            "INSERT INTO pipeline_git_hooks
             (id, pipeline_id, provider, hook_key, secret_ciphertext, branch_filter, enabled)
             VALUES ('hook-2', 'pipe-a', 'github', 'key-2', 'cipher-2', '', 1)",
        )
        .execute(&pool)
        .await;
        assert!(
            second.is_ok(),
            "multiple hooks per pipeline must be allowed after migration"
        );
    }

    #[tokio::test]
    async fn environment_bindings_survive_the_rebuild() {
        let pool = pool().await;
        seed_pipelines(&pool, &["pipe-a", "pipe-b"]).await;
        // Mid-schema: UNIQUE pipeline_id, but `environment` already added by ALTER.
        sqlx::query(
            "CREATE TABLE pipeline_git_hooks (
                id TEXT PRIMARY KEY,
                pipeline_id TEXT NOT NULL UNIQUE,
                provider TEXT NOT NULL,
                hook_key TEXT NOT NULL UNIQUE,
                secret_ciphertext TEXT NOT NULL,
                branch_filter TEXT NOT NULL DEFAULT '',
                environment TEXT NOT NULL DEFAULT '',
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO pipeline_git_hooks
             (id, pipeline_id, provider, hook_key, secret_ciphertext, branch_filter, environment, enabled)
             VALUES ('hook-1', 'pipe-a', 'github', 'key-1', 'cipher-1', 'main', 'prod', 1),
                    ('hook-2', 'pipe-b', 'gitee', 'key-2', 'cipher-2', '', '', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        migrate_pipeline_git_hooks_multi(&pool).await.unwrap();

        let env_a: String =
            sqlx::query_scalar("SELECT environment FROM pipeline_git_hooks WHERE id = 'hook-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let env_b: String =
            sqlx::query_scalar("SELECT environment FROM pipeline_git_hooks WHERE id = 'hook-2'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(env_a, "prod", "explicit environment binding is preserved");
        assert_eq!(env_b, "");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pipeline_git_hooks")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2, "no rows lost in the rebuild");
    }

    #[tokio::test]
    async fn current_schema_is_left_untouched() {
        let pool = pool().await;
        seed_pipelines(&pool, &["pipe-a"]).await;
        sqlx::query(
            "CREATE TABLE pipeline_git_hooks (
                id TEXT PRIMARY KEY,
                pipeline_id TEXT NOT NULL,
                label TEXT NOT NULL DEFAULT '',
                provider TEXT NOT NULL,
                hook_key TEXT NOT NULL UNIQUE,
                secret_ciphertext TEXT NOT NULL,
                branch_filter TEXT NOT NULL DEFAULT '',
                environment TEXT NOT NULL DEFAULT '',
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO pipeline_git_hooks
             (id, pipeline_id, label, provider, hook_key, secret_ciphertext, environment, enabled)
             VALUES ('hook-1', 'pipe-a', 'deploy', 'github', 'key-1', 'cipher-1', 'prod', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        migrate_pipeline_git_hooks_multi(&pool).await.unwrap();

        let untouched: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'pipeline_git_hooks'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            !untouched.contains("UNIQUE,\n                provider")
                && untouched.contains("environment"),
            "current schema must not be rebuilt"
        );
        let label: String =
            sqlx::query_scalar("SELECT label FROM pipeline_git_hooks WHERE id = 'hook-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(label, "deploy");
    }
}

/// Cut over `tasks` config columns to Config Block DSL v2 names (one-shot, no dual-read).
///
/// `config_path` → `config_key_path`, `config_dir` → `config_dest`,
/// `env_render` → `config_render` (JSON array; wrap legacy single object).
async fn tasks_has_column(pool: &SqlitePool, name: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('tasks') WHERE name = ?",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}

async fn migrate_tasks_config_block_columns(pool: &SqlitePool) -> anyhow::Result<()> {
    use sqlx::Row;

    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN config_key_path TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN config_dest TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN config_render TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    let _ =
        sqlx::query("ALTER TABLE tasks ADD COLUMN hosts_strategy TEXT NOT NULL DEFAULT 'chain'")
            .execute(pool)
            .await;

    if tasks_has_column(pool, "config_path").await {
        sqlx::query(
            "UPDATE tasks SET config_key_path = config_path \
             WHERE config_key_path = '' AND config_path != ''",
        )
        .execute(pool)
        .await?;
    }
    if tasks_has_column(pool, "config_dir").await {
        sqlx::query(
            "UPDATE tasks SET config_dest = config_dir \
             WHERE config_dest = '' AND config_dir != ''",
        )
        .execute(pool)
        .await?;
    }
    if tasks_has_column(pool, "env_render").await {
        let rows = sqlx::query(
            "SELECT id, env_render FROM tasks \
             WHERE env_render != '' AND (config_render = '' OR config_render IS NULL)",
        )
        .fetch_all(pool)
        .await?;
        for row in rows {
            let id: String = row.get(0);
            let raw: String = row.get(1);
            let normalized = match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(serde_json::Value::Array(_)) => raw,
                Ok(serde_json::Value::Object(_)) => format!("[{raw}]"),
                Ok(other) if !other.is_null() => {
                    serde_json::to_string(&serde_json::json!([other])).unwrap_or_default()
                }
                _ => String::new(),
            };
            if !normalized.is_empty() {
                sqlx::query("UPDATE tasks SET config_render = ? WHERE id = ?")
                    .bind(&normalized)
                    .bind(&id)
                    .execute(pool)
                    .await?;
            }
        }
    }

    for legacy in ["config_path", "config_dir", "env_render"] {
        if tasks_has_column(pool, legacy).await {
            let q = format!("ALTER TABLE tasks DROP COLUMN {legacy}");
            match sqlx::query(&q).execute(pool).await {
                Ok(_) => tracing::info!("Dropped tasks.{legacy} (config-block v2 cut-over)"),
                Err(e) => tracing::warn!("Could not DROP tasks.{legacy}: {e}"),
            }
        }
    }
    Ok(())
}

/// Rebuild `pipeline_git_hooks` when an older schema enforced one hook per pipeline.
/// Also recovers from a half-finished prior attempt (`pipeline_git_hooks_new` left behind).
async fn migrate_pipeline_git_hooks_multi(pool: &SqlitePool) -> anyhow::Result<()> {
    let has_main: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'pipeline_git_hooks'",
    )
    .fetch_one(pool)
    .await?;
    let has_new: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'pipeline_git_hooks_new'",
    )
    .fetch_one(pool)
    .await?;

    // Recovery: previous run dropped main then failed on RENAME.
    if !has_main && has_new {
        tracing::warn!("Recovering pipeline_git_hooks from leftover pipeline_git_hooks_new");
        sqlx::query("ALTER TABLE pipeline_git_hooks_new RENAME TO pipeline_git_hooks")
            .execute(pool)
            .await?;
        return Ok(());
    }

    // Both exist: prefer data in `_new` if main is empty; otherwise drop orphan `_new`.
    if has_main && has_new {
        let main_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pipeline_git_hooks")
            .fetch_one(pool)
            .await?;
        let new_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pipeline_git_hooks_new")
            .fetch_one(pool)
            .await?;
        if main_count == 0 && new_count > 0 {
            tracing::warn!("Replacing empty pipeline_git_hooks with pipeline_git_hooks_new");
            sqlx::query("DROP TABLE pipeline_git_hooks")
                .execute(pool)
                .await?;
            sqlx::query("ALTER TABLE pipeline_git_hooks_new RENAME TO pipeline_git_hooks")
                .execute(pool)
                .await?;
            return Ok(());
        }
        tracing::warn!("Dropping orphaned pipeline_git_hooks_new");
        sqlx::query("DROP TABLE IF EXISTS pipeline_git_hooks_new")
            .execute(pool)
            .await?;
    }

    let sql: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'pipeline_git_hooks'",
    )
    .fetch_optional(pool)
    .await?;
    let Some(sql) = sql else {
        return Ok(());
    };
    // Old CREATE used `pipeline_id TEXT NOT NULL UNIQUE`
    let needs =
        sql.contains("pipeline_id TEXT NOT NULL UNIQUE") || sql.contains("pipeline_id TEXT UNIQUE");
    if !needs {
        return Ok(());
    }

    tracing::info!("Migrating pipeline_git_hooks to allow multiple hooks per pipeline");
    sqlx::query("DROP TABLE IF EXISTS pipeline_git_hooks_new")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE TABLE pipeline_git_hooks_new (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            label TEXT NOT NULL DEFAULT '',
            provider TEXT NOT NULL,
            hook_key TEXT NOT NULL UNIQUE,
            secret_ciphertext TEXT NOT NULL,
            branch_filter TEXT NOT NULL DEFAULT '',
            environment TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY (pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        )",
    )
    .execute(pool)
    .await?;

    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('pipeline_git_hooks')")
            .fetch_all(pool)
            .await?;
    let has_label = columns.iter().any(|column| column == "label");
    let has_environment = columns.iter().any(|column| column == "environment");
    let label = if has_label {
        "COALESCE(label, '')"
    } else {
        "''"
    };
    let environment = if has_environment {
        "COALESCE(environment, '')"
    } else {
        "''"
    };
    sqlx::query(&format!(
        "INSERT INTO pipeline_git_hooks_new
         (id, pipeline_id, label, provider, hook_key, secret_ciphertext, branch_filter, environment, enabled, created_at, updated_at)
         SELECT id, pipeline_id, {label}, provider, hook_key, secret_ciphertext,
                branch_filter, {environment}, enabled, created_at, updated_at
         FROM pipeline_git_hooks",
    ))
    .execute(pool)
    .await?;

    // Drop indexes that can block rename on some SQLite builds, then swap tables.
    let _ = sqlx::query("DROP INDEX IF EXISTS idx_pipeline_git_hooks_key")
        .execute(pool)
        .await;
    let _ = sqlx::query("DROP INDEX IF EXISTS idx_pipeline_git_hooks_pipeline")
        .execute(pool)
        .await;

    sqlx::query("DROP TABLE pipeline_git_hooks")
        .execute(pool)
        .await?;
    sqlx::query("ALTER TABLE pipeline_git_hooks_new RENAME TO pipeline_git_hooks")
        .execute(pool)
        .await?;
    Ok(())
}

/// Mark existing secrets whose ciphertext decrypts to registry JSON as `kind=registry`.
async fn backfill_registry_secret_kind(pool: &SqlitePool, jwt_secret: &str) -> anyhow::Result<()> {
    let rows = sqlx::query("SELECT id, value, kind FROM secrets")
        .fetch_all(pool)
        .await?;
    let mut updated = 0u32;
    for row in rows {
        use sqlx::Row;
        let id: String = row.get(0);
        let stored: String = row.get(1);
        let kind: String = row.get(2);
        if shared::secret_kind::is_registry_kind(&kind) {
            continue;
        }
        let Ok(plain) = crate::crypto::decrypt(&stored, jwt_secret) else {
            continue;
        };
        if !shared::registry_auth::looks_like_registry_secret_value(&plain) {
            continue;
        }
        sqlx::query("UPDATE secrets SET kind = 'registry' WHERE id = ?")
            .bind(&id)
            .execute(pool)
            .await?;
        updated += 1;
    }
    if updated > 0 {
        tracing::info!("Backfilled kind=registry on {updated} secret(s)");
    }
    Ok(())
}

/// Assign run numbers to jobs that predate the feature (run_number = 0), in
/// created_at order per pipeline, then move each pipeline's allocation cursor
/// past its history. Idempotent: numbered rows are left untouched.
async fn backfill_job_run_numbers(pool: &SqlitePool) -> anyhow::Result<()> {
    // Number the unnumbered jobs (oldest first) per pipeline.
    sqlx::query(
        "WITH numbered AS (
             SELECT id, ROW_NUMBER() OVER (
                 PARTITION BY pipeline_id ORDER BY created_at, id) AS rn
             FROM jobs WHERE run_number = 0
         )
         UPDATE jobs SET run_number = (
             SELECT numbered.rn FROM numbered WHERE numbered.id = jobs.id
         ) WHERE run_number = 0",
    )
    .execute(pool)
    .await?;
    // Advance each pipeline's cursor past its backfilled history.
    sqlx::query(
        "UPDATE pipelines SET run_counter = COALESCE(\
             (SELECT MAX(run_number) FROM jobs WHERE jobs.pipeline_id = pipelines.id), 0)",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Atomically allocate the next per-pipeline build number. Safe under
/// concurrent triggers: the counter bumps inside the single UPDATE.
pub async fn allocate_run_number(
    conn: &mut sqlx::SqliteConnection,
    pipeline_id: &str,
) -> anyhow::Result<i64> {
    let next: i64 = sqlx::query_scalar(
        "UPDATE pipelines SET run_counter = run_counter + 1 \
         WHERE id = ? RETURNING run_counter",
    )
    .bind(pipeline_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(next)
}
