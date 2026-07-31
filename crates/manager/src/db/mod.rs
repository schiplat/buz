use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

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
    let sql = include_str!("migrations/001_init.sql");

    // Split by ; and execute each statement
    for statement in sql.split(';') {
        let trimmed = statement.trim();
        if !trimmed.is_empty() {
            sqlx::query(trimmed).execute(pool).await?;
        }
    }

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

    // Add is_frozen column to runners table (safe to fail if already present)
    let _ = sqlx::query("ALTER TABLE runners ADD COLUMN is_frozen INTEGER NOT NULL DEFAULT 0")
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
        "ALTER TABLE runners ADD COLUMN ssh_host_key_policy TEXT NOT NULL DEFAULT 'insecure'",
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

    // Add step_statuses column to jobs table (safe to fail if already present)
    let _ = sqlx::query("ALTER TABLE jobs ADD COLUMN step_statuses TEXT NOT NULL DEFAULT '[]'")
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

    // Add step_secrets column to tasks — per-step secret scoping
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN step_secrets TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

    // Private registry credentials secret name (docker / shell / SSH)
    let _ = sqlx::query("ALTER TABLE tasks ADD COLUMN registry_secret TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;

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

    backfill_registry_secret_kind(pool, jwt_secret).await?;

    tracing::info!("Database migrations applied successfully");
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
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY (pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        )",
    )
    .execute(pool)
    .await?;

    // Old table may lack `label`
    let has_label = sql.to_ascii_lowercase().contains("label");
    if has_label {
        sqlx::query(
            "INSERT INTO pipeline_git_hooks_new
             (id, pipeline_id, label, provider, hook_key, secret_ciphertext, branch_filter, enabled, created_at, updated_at)
             SELECT id, pipeline_id, COALESCE(label, ''), provider, hook_key, secret_ciphertext,
                    branch_filter, enabled, created_at, updated_at
             FROM pipeline_git_hooks",
        )
        .execute(pool)
        .await?;
    } else {
        sqlx::query(
            "INSERT INTO pipeline_git_hooks_new
             (id, pipeline_id, label, provider, hook_key, secret_ciphertext, branch_filter, enabled, created_at, updated_at)
             SELECT id, pipeline_id, '', provider, hook_key, secret_ciphertext,
                    branch_filter, enabled, created_at, updated_at
             FROM pipeline_git_hooks",
        )
        .execute(pool)
        .await?;
    }

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
