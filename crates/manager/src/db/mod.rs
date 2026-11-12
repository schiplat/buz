use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};

/// Postgres migration runner (advisory-locked baseline application). Declared
/// only under the `postgres` feature — default builds carry no PG driver code.
#[cfg(feature = "postgres")]
pub mod migrate_pg;

use crate::config::ClusterMode;
use crate::config::Config;
use crate::config::{DatabaseConfig, StorageKind};

/// The SQLite file this config points at: `url` (`sqlite:<path>`) when set,
/// otherwise the legacy `[database] path`. The startup gate refuses other
/// schemes before this is reached.
fn sqlite_file(database: &DatabaseConfig) -> String {
    match &database.url {
        Some(url) if url.trim().to_lowercase().starts_with("sqlite:") => url.trim()
            [("sqlite:".len())..]
            .trim_start_matches("//")
            .to_string(),
        _ => database.path.clone(),
    }
}

/// Refuse to start when `[cluster] mode` and the storage backend contradict
/// each other, or when the configured backend is not wired.
///
/// - `multi` + SQLite is refused: SQLite has no cross-instance coordination
///   (a single writer, no row locks), so two Managers on one file would
///   corrupt scheduling state. `multi` needs the Postgres backend.
/// - Postgres is refused until the PG store backend lands: the config parses,
///   but no code path can serve it yet — fail closed rather than half-work.
pub fn validate_cluster_mode(mode: ClusterMode, database: &DatabaseConfig) -> anyhow::Result<()> {
    let kind = database.storage_kind().map_err(anyhow::Error::msg)?;
    match (mode, kind) {
        (ClusterMode::Single, StorageKind::Sqlite) => Ok(()),
        (ClusterMode::Multi, StorageKind::Sqlite) => Err(anyhow::anyhow!(
            "[cluster] mode = \"multi\" requires the Postgres backend: SQLite has no \
             cross-instance coordination (single writer, no row locks), so two Manager \
             instances on one SQLite file would corrupt scheduling state. Point \
             [database] url at postgres:, or keep mode = \"single\"."
        )),
        (_, StorageKind::Postgres) => Err(anyhow::anyhow!(
            "database url points at Postgres, but that backend is not wired yet \
             (store-boundary follow-up). Run SQLite with [cluster] mode = \"single\" \
             meanwhile."
        )),
    }
}

/// Held-for-lifetime proof that this process is the only Manager serving its
/// storage: an flock on a `<db path>.lock` sidecar. Dropping the handle (or
/// exiting) releases it. Postgres needs no file lock — its instance lock is a
/// session advisory lock, which arrives with the PG backend.
#[derive(Debug)]
pub struct InstanceLock {
    _file: Option<std::fs::File>,
}

/// Take the single-instance lock for the configured storage. Errors when
/// another process already holds it.
pub fn acquire_instance_lock(database: &DatabaseConfig) -> anyhow::Result<InstanceLock> {
    let kind = database.storage_kind().map_err(anyhow::Error::msg)?;
    if kind == StorageKind::Postgres {
        // No file lock for PG; the session advisory lock arrives with the
        // Postgres backend.
        return Ok(InstanceLock { _file: None });
    }
    let lock_path = format!("{}.lock", sqlite_file(database));
    if let Some(parent) = std::path::Path::new(&lock_path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    if file.try_lock().is_err() {
        return Err(anyhow::anyhow!(
            "another Manager instance is already serving {lock_path} — refusing to \
             start a second one; stop it first (systemd: `systemctl stop buz-manager`)"
        ));
    }
    Ok(InstanceLock { _file: Some(file) })
}

pub async fn init(config: &Config) -> anyhow::Result<SqlitePool> {
    let db_file = sqlite_file(&config.database);
    // Ensure data directory exists
    if let Some(parent) = std::path::Path::new(&db_file).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&format!("sqlite:{db_file}?mode=rwc"))
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

/// Read one Manager-local fact from `manager_meta`.
pub async fn manager_meta_get(pool: &SqlitePool, key: &str) -> anyhow::Result<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT value FROM manager_meta WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await?,
    )
}

/// Persist one Manager-local fact in `manager_meta` (upsert).
pub async fn manager_meta_set(pool: &SqlitePool, key: &str, value: &str) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO manager_meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(key)
        .bind(value)
        .execute(pool)
        .await?;
    Ok(())
}

/// `manager_meta` key holding the persistent Manager control-plane identity.
pub const META_KEY_MANAGER_INSTANCE_ID: &str = "manager_instance_id";

/// Resolve the Manager instance identity: `BUZ_MANAGER_INSTANCE_ID` (env,
/// pinned by the operator) > the value persisted in `manager_meta` on a
/// previous boot > generate `mgr-<uuid>` once and persist it.
///
/// Multiple Managers on different devices are independent control planes (the
/// SQLite database is local to one device); this identity binds ownership and
/// log/audit correlation — it is not an HA coordinator.
pub async fn ensure_manager_instance_id(pool: &SqlitePool) -> anyhow::Result<String> {
    const ENV_VAR: &str = "BUZ_MANAGER_INSTANCE_ID";
    match shared::instance::from_env(ENV_VAR) {
        Ok(Some(id)) => {
            manager_meta_set(pool, META_KEY_MANAGER_INSTANCE_ID, &id).await?;
            Ok(id)
        }
        Ok(None) => {
            if let Some(existing) = manager_meta_get(pool, META_KEY_MANAGER_INSTANCE_ID).await? {
                if let Ok(id) = shared::instance::validate(&existing) {
                    return Ok(id);
                }
                tracing::warn!(stored = %existing, "persisted manager_instance_id is invalid; regenerating");
            }
            let id = shared::instance::validate(&format!("mgr-{}", uuid::Uuid::new_v4()))
                .map_err(anyhow::Error::msg)?;
            manager_meta_set(pool, META_KEY_MANAGER_INSTANCE_ID, &id).await?;
            Ok(id)
        }
        Err(error) => Err(anyhow::anyhow!("invalid {ENV_VAR}: {error}")),
    }
}

/// Add a column unless it already exists. `ALTER TABLE ... ADD COLUMN` has no
/// `IF NOT EXISTS` in SQLite, so migrations are written idempotently by
/// ignoring the duplicate-column error — but ONLY that error: a wrong table
/// name or a statement ordered before its `CREATE TABLE` used to fail
/// silently, which is how a column can be "migrated" yet absent.
/// Rename a table when the old name still exists and the new one does not.
/// Table renames are not expressible as `CREATE TABLE IF NOT EXISTS`, so an
/// existing deployment would otherwise keep querying the old name and fail.
async fn rename_table_if_present(pool: &SqlitePool, from: &str, to: &str) {
    let exists = |name: &str| {
        let name = name.to_string();
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
            )
            .bind(name)
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .is_some()
        }
    };
    if !exists(from).await || exists(to).await {
        return;
    }
    let sql = format!("ALTER TABLE {from} RENAME TO {to}");
    if let Err(error) = sqlx::query(&sql).execute(pool).await {
        tracing::warn!(%sql, %error, "table rename failed");
    }
}

async fn add_column_if_missing(pool: &SqlitePool, table: &str, definition: &str) {
    let sql = format!("ALTER TABLE {table} ADD COLUMN {definition}");
    if let Err(error) = sqlx::query(&sql).execute(pool).await {
        let message = error.to_string();
        if !message.contains("duplicate column name") {
            tracing::warn!(%sql, %message, "migration statement failed");
        }
    }
}

/// Rename a column when the old name is present (idempotent: a no-op once
/// renamed or on fresh databases created with the new name).
async fn rename_column_if_present(pool: &SqlitePool, table: &str, from: &str, to: &str) {
    let has_old: bool =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = ?")
            .bind(table)
            .bind(from)
            .fetch_one(pool)
            .await
            .unwrap_or(0)
            != 0;
    if !has_old {
        return;
    }
    let sql = format!("ALTER TABLE {table} RENAME COLUMN {from} TO {to}");
    if let Err(error) = sqlx::query(&sql).execute(pool).await {
        tracing::warn!(%sql, %error, "migration statement failed");
    }
}

/// One-time data migration behind the whitelist retirement (2026-10): tenant
/// policy moved from the per-host `sandbox_host_teams` whitelist onto the
/// machine's Runner team bindings (`runners.team_id`). For each host instance
/// whose whitelist holds exactly one still-existing team, every attached
/// runner without a team binding adopts it. Multi-team whitelists are
/// ambiguous (a machine maps to one runner team) and are dropped — affected
/// hosts become open and the operator reconciles via runner bindings. Finally
/// the table is removed. Idempotent: a no-op once the table is gone.
async fn migrate_host_whitelists_to_runners(pool: &SqlitePool) {
    let table_exists: bool = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'sandbox_host_teams'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0)
        != 0;
    if !table_exists {
        return;
    }

    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT host_id, instance_id, team_id FROM sandbox_host_teams ORDER BY team_id",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut by_pair: std::collections::HashMap<(String, String), Vec<String>> =
        std::collections::HashMap::new();
    for (host_id, instance_id, team_id) in rows {
        by_pair
            .entry((host_id, instance_id))
            .or_default()
            .push(team_id);
    }

    let mut migrated: u64 = 0;
    for ((host_id, instance_id), teams) in &by_pair {
        // Exactly one team (that still exists) can map onto runner bindings.
        if teams.len() != 1 {
            tracing::warn!(
                host = %host_id,
                instance = %instance_id,
                teams = teams.len(),
                "host whitelist had multiple teams — dropped; bind runners manually"
            );
            continue;
        }
        let team_id = &teams[0];
        let team_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM teams WHERE id = ?)")
                .bind(team_id)
                .fetch_one(pool)
                .await
                .unwrap_or(false);
        if !team_exists {
            tracing::warn!(host = %host_id, team = %team_id, "whitelist team no longer exists — dropped");
            continue;
        }
        let result = sqlx::query(
            "UPDATE runners SET team_id = ?, updated_at = datetime('now')
             WHERE host_id = ? AND sandbox_instance_id = ? AND team_id IS NULL",
        )
        .bind(team_id)
        .bind(host_id)
        .bind(instance_id)
        .execute(pool)
        .await;
        match result {
            Ok(res) if res.rows_affected() > 0 => {
                migrated += res.rows_affected();
                tracing::info!(host = %host_id, instance = %instance_id, team = %team_id,
                    "migrated host whitelist onto runner team bindings");
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, host = %host_id, "whitelist migration update failed")
            }
        }
    }

    if let Err(error) = sqlx::query("DROP TABLE IF EXISTS sandbox_host_teams")
        .execute(pool)
        .await
    {
        tracing::warn!(%error, "failed to drop sandbox_host_teams");
    }
    if migrated > 0 {
        tracing::info!(
            migrated,
            "host whitelists retired onto runner team bindings"
        );
    }
}

/// Ordered schema migrations. All v1.x schema lives in version 1 as one body:
/// before versioning existed, every startup replayed the whole baseline
/// (`001`–`003` DDL + the additive/`pragma_table_info` chain + data
/// migrations), so deployed databases already hold all of it and each
/// statement is idempotent by construction.
///
/// To add a migration: append a version, add its body to `MIGRATIONS`
/// dispatch below, and **never edit or reorder an existing entry** — its
/// version is already recorded on deployed databases, so an edited body would
/// silently never run. New bodies need not be idempotent: they run once.
const MIGRATIONS: &[i64] = &[1, 2, 3];

/// Versioned migration runner: applies each migration at most once per
/// database, recording it in `schema_migrations`.
///
/// Only single-writer deployment is assumed today: a concurrent second Manager
/// on the same SQLite file could interleave a migration. Serialising that is
/// the job of the storage switch (Postgres advisory lock); with SQLite `WAL`
/// and one Manager there is no second writer.
pub async fn run_migrations(pool: &SqlitePool, jwt_secret: &str) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    let applied: Vec<i64> = sqlx::query_scalar("SELECT version FROM schema_migrations")
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    for version in MIGRATIONS {
        if applied.contains(version) {
            tracing::debug!(version, "migration already applied — skipped");
            continue;
        }
        tracing::info!(version, "applying migration");
        apply_migration(pool, jwt_secret, *version).await?;
        sqlx::query("INSERT INTO schema_migrations (version) VALUES (?)")
            .bind(version)
            .execute(pool)
            .await?;
    }

    tracing::info!("Database migrations applied successfully");
    Ok(())
}

async fn apply_migration(pool: &SqlitePool, jwt_secret: &str, version: i64) -> anyhow::Result<()> {
    match version {
        1 => apply_legacy_baseline(pool, jwt_secret).await,
        2 => apply_v2_yaml_source(pool).await,
        3 => apply_v3_insertion_order_seq(pool).await,
        other => anyhow::bail!("unknown migration version {other}"),
    }
}

/// v2: the canonical YAML text moves into the database. The file under
/// `pipeline.yaml_scan_dir` stops being authoritative in this release (the
/// directory scan is retired); `yaml_path` keeps only the origin basename for
/// display and export, and `yaml_source` holds the exact bytes the API
/// validated.
async fn apply_v2_yaml_source(pool: &SqlitePool) -> anyhow::Result<()> {
    add_column_if_missing(pool, "pipelines", "yaml_source TEXT NOT NULL DEFAULT ''").await;
    // Origin basenames identify a stored definition. The API pre-checks before
    // writing; this index makes two concurrent creates converge on exactly one
    // winner instead of two rows sharing a definition name.
    //
    // A pre-versioning database that already holds duplicates cannot take the
    // index. Name them, so the operator can fix the rows instead of reading a
    // bare constraint error.
    let duplicates: Vec<(String, i64)> = sqlx::query_as(
        "SELECT yaml_path, COUNT(*) FROM pipelines
         WHERE source = 'yaml' AND yaml_path IS NOT NULL AND yaml_path != ''
         GROUP BY yaml_path HAVING COUNT(*) > 1",
    )
    .fetch_all(pool)
    .await?;
    if !duplicates.is_empty() {
        let list = duplicates
            .iter()
            .map(|(path, count)| format!("{path} ×{count}"))
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "cannot enforce one pipeline per definition name — duplicate yaml_path rows: {list}. \
             Delete or re-save the extra pipelines, then restart."
        );
    }
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_pipelines_yaml_path
         ON pipelines(yaml_path) WHERE source = 'yaml'",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// v3: explicit insertion-order sequences for `jobs` and `audit_logs`.
///
/// Business logic picks "the newest pending job" and orders FIFO candidates by
/// `rowid` — an SQLite-only implicit column with no Postgres equivalent. Those
/// orderings move to an explicit `seq` maintained by a trigger, so the queries
/// become dialect-neutral (`ORDER BY seq`).
///
/// Postgres mapping (storage switch): `seq BIGINT GENERATED ALWAYS AS
/// IDENTITY` replaces both the column default and the trigger.
///
/// Trigger internals use `NEW.id` (both tables are `id TEXT PRIMARY KEY`), so
/// no `rowid` dependency enters the schema objects either. The `MAX(seq)`
/// subquery is index-backed (`idx_jobs_seq` / `idx_audit_logs_seq`); SQLite's
/// single-writer rule makes the read-increment-write inside the trigger safe.
async fn apply_v3_insertion_order_seq(pool: &SqlitePool) -> anyhow::Result<()> {
    add_column_if_missing(pool, "jobs", "seq INTEGER NOT NULL DEFAULT 0").await;
    add_column_if_missing(pool, "audit_logs", "seq INTEGER NOT NULL DEFAULT 0").await;

    // Backfill from rowid: it encoded insertion order until now. UPDATE does
    // not fire the INSERT triggers below, so order of operations is free.
    sqlx::query("UPDATE jobs SET seq = rowid WHERE seq = 0")
        .execute(pool)
        .await?;
    sqlx::query("UPDATE audit_logs SET seq = rowid WHERE seq = 0")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_jobs_seq ON jobs(seq)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_audit_logs_seq ON audit_logs(seq)")
        .execute(pool)
        .await?;

    // WHEN NEW.seq = 0 leaves explicitly-numbered inserts (tests, future
    // import tools) untouched.
    sqlx::query(
        "CREATE TRIGGER IF NOT EXISTS trg_jobs_insert_seq
         AFTER INSERT ON jobs
         WHEN NEW.seq = 0
         BEGIN
             UPDATE jobs SET seq = (SELECT COALESCE(MAX(seq), 0) + 1 FROM jobs)
             WHERE id = NEW.id;
         END",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TRIGGER IF NOT EXISTS trg_audit_logs_insert_seq
         AFTER INSERT ON audit_logs
         WHEN NEW.seq = 0
         BEGIN
             UPDATE audit_logs SET seq = (SELECT COALESCE(MAX(seq), 0) + 1 FROM audit_logs)
             WHERE id = NEW.id;
         END",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// The 1.x baseline, preserved verbatim from the pre-versioning runner.
async fn apply_legacy_baseline(pool: &SqlitePool, _jwt_secret: &str) -> anyhow::Result<()> {
    // NOTE: the per-host team whitelist (sandbox_host_teams) is retired before
    // it ever shipped — tenant policy lives on runner team bindings. Dev
    // databases that still hold the table are migrated and the table dropped
    // by migrate_host_whitelists_to_runners below; fresh installs never create it.
    for path in [
        include_str!("migrations/001_init.sql"),
        include_str!("migrations/002_step_resolutions.sql"),
        include_str!("migrations/003_sandbox_hosts.sql"),
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
    add_column_if_missing(pool, "pipelines", "yaml_sha256 TEXT NOT NULL DEFAULT ''").await;
    add_column_if_missing(
        pool,
        "pipelines",
        "cron_last_fired_at TEXT NOT NULL DEFAULT ''",
    )
    .await;

    // Add snapshot column to existing databases (safe to fail if already present)
    add_column_if_missing(pool, "jobs", "snapshot TEXT NOT NULL DEFAULT '{}'").await;

    // Add system_info column to runners table (safe to fail if already present)
    add_column_if_missing(pool, "runners", "system_info TEXT NOT NULL DEFAULT '{}'").await;

    // Runner-reported deployment identities (see shared::instance): the host
    // machine id and this Runner installation's instance id. Empty on runners
    // that have not reported yet; both are public identifiers, never secrets.
    add_column_if_missing(pool, "runners", "host_id TEXT NOT NULL DEFAULT ''").await;
    add_column_if_missing(
        pool,
        "runners",
        "runner_instance_id TEXT NOT NULL DEFAULT ''",
    )
    .await;

    // Resource profile requests (sandbox-host-configuration design record).
    // pipelines.resource_profile: the YAML `runner.resources` name ('' = default).
    // jobs.resource_profile: frozen at trigger after the team grant check;
    // jobs.resource_slots / resource_spec / sandbox_instance_id: filled at
    // claim from the dispatching host's applied document (scheduling PR).
    add_column_if_missing(
        pool,
        "pipelines",
        "resource_profile TEXT NOT NULL DEFAULT ''",
    )
    .await;
    add_column_if_missing(pool, "jobs", "resource_profile TEXT NOT NULL DEFAULT ''").await;
    add_column_if_missing(pool, "jobs", "resource_slots INTEGER NOT NULL DEFAULT 1").await;
    add_column_if_missing(pool, "jobs", "resource_spec TEXT NOT NULL DEFAULT '{}'").await;
    add_column_if_missing(pool, "jobs", "sandbox_instance_id TEXT NOT NULL DEFAULT ''").await;
    // Why a pending job was last skipped by a poller (slot wait, profile not
    // offered, host mode); cleared on claim. Display-only.
    add_column_if_missing(pool, "jobs", "dispatch_reason TEXT NOT NULL DEFAULT ''").await;
    add_column_if_missing(pool, "jobs", "logs_purged INTEGER NOT NULL DEFAULT 0").await;
    // Slot-aware claim: UI-managed walk strategy and the heartbeat-derived
    // sandboxd instance this Runner is attached to.
    add_column_if_missing(
        pool,
        "runners",
        "claim_strategy TEXT NOT NULL DEFAULT 'backfill'",
    )
    .await;
    add_column_if_missing(
        pool,
        "runners",
        "sandbox_instance_id TEXT NOT NULL DEFAULT ''",
    )
    .await;

    // ── Retire the per-host team whitelist (sandbox_host_teams) ──
    // Tenant policy lives on the Runner (runners.team_id, enforced in the
    // poll loop). For each host instance whose whitelist holds exactly one
    // still-existing team, bind every attached runner (team_id IS NULL) to
    // that team; multi-team whitelists cannot map to one runner team and are
    // dropped — affected hosts become open and the operator reconciles via
    // runner bindings. Then the table is removed. Runs after the
    // sandbox_instance_id column is ensured (the mapping needs it).
    migrate_host_whitelists_to_runners(pool).await;

    // Add privileged column to tasks table (safe to fail if already present)
    add_column_if_missing(pool, "tasks", "privileged INTEGER NOT NULL DEFAULT 0").await;

    // Add input_defs column to pipelines table (safe to fail if already present)
    add_column_if_missing(pool, "pipelines", "input_defs TEXT NOT NULL DEFAULT '[]'").await;

    // Add level column to job_logs (chunk severity; NULL = legacy chunk)
    add_column_if_missing(pool, "job_logs", "level TEXT").await;

    migrate_step_resolutions(pool).await?;

    // Add webhook_secret column (bare team secret name; empty = no verification)
    add_column_if_missing(pool, "pipelines", "webhook_secret TEXT NOT NULL DEFAULT ''").await;

    // Add is_frozen column to runners table (safe to fail if already present)
    add_column_if_missing(pool, "runners", "is_frozen INTEGER NOT NULL DEFAULT 0").await;

    // One-shot restart flag: set by POST /runners/{id}/restart, served via the
    // next heartbeat, then cleared. Also blocks dispatch (like freeze) during
    // the request window so pending jobs don't sneak in before the restart lands.
    let _ =
        sqlx::query("ALTER TABLE runners ADD COLUMN restart_requested INTEGER NOT NULL DEFAULT 0")
            .execute(pool)
            .await;

    // One-shot capability re-probe flag: set by
    // POST /runners/{id}/reprobe-capabilities, served via the next heartbeat,
    // then cleared. Unlike restart it does not block dispatch — probing is
    // observational.
    let _ = sqlx::query(
        "ALTER TABLE runners ADD COLUMN capability_reprobe_requested INTEGER NOT NULL DEFAULT 0",
    )
    .execute(pool)
    .await;
    // UI state machine: requested_at marks the queue moment, served_at the
    // moment the Runner's heartbeat picked the command up (results land in a
    // later heartbeat; the dashboard compares against capabilities.checked_at).
    let _ = sqlx::query("ALTER TABLE runners ADD COLUMN capability_reprobe_requested_at TEXT")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE runners ADD COLUMN capability_reprobe_served_at TEXT")
        .execute(pool)
        .await;
    // Decision notes recorded at approve/reject time (shown in the queue history).
    let _ = sqlx::query("ALTER TABLE sandbox_config_changes ADD COLUMN decision_comment TEXT")
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
    add_column_if_missing(pool, "runners", "shell_enabled INTEGER NOT NULL DEFAULT 1").await;
    add_column_if_missing(pool, "runners", "ssh_enabled INTEGER NOT NULL DEFAULT 1").await;
    add_column_if_missing(pool, "runners", "docker_enabled INTEGER NOT NULL DEFAULT 0").await;
    // UI policy (Runner Policy page): isolate shell steps in a loopback-only
    // Linux network namespace. Default off.
    add_column_if_missing(
        pool,
        "runners",
        "netns_isolation INTEGER NOT NULL DEFAULT 0",
    )
    .await;
    add_column_if_missing(pool, "runners", "wasm_enabled INTEGER NOT NULL DEFAULT 0").await;
    let _ = sqlx::query(
        "ALTER TABLE runners ADD COLUMN ssh_host_key_policy TEXT NOT NULL DEFAULT 'known_hosts'",
    )
    .execute(pool)
    .await;

    // Add host_labels column to tasks table (safe to fail if already present)
    add_column_if_missing(pool, "tasks", "host_labels TEXT NOT NULL DEFAULT ''").await;

    // Docker engine: container image for uses=docker steps
    add_column_if_missing(pool, "tasks", "image TEXT NOT NULL DEFAULT ''").await;

    // Docker engine: extra host→container volume mounts (JSON array or comma-separated)
    add_column_if_missing(pool, "tasks", "volumes TEXT NOT NULL DEFAULT ''").await;

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
    add_column_if_missing(pool, "jobs", "step_statuses TEXT NOT NULL DEFAULT '[]'").await;

    // Admin-only debug flag: when set, the Runner skips log redaction for this job.
    // Dispatched to the Runner and used to gate debug-job log reads to admins.
    add_column_if_missing(pool, "jobs", "debug_unredact INTEGER NOT NULL DEFAULT 0").await;

    // Verbose execution mode: any user can trigger with extra detail in the
    // Runner's logs (per-step duration, exit code, stage markers). Redaction
    // stays fully intact — distinct from the admin-only debug_unredact flag.
    add_column_if_missing(pool, "jobs", "verbose_logs INTEGER NOT NULL DEFAULT 0").await;

    // Add owner_group column to secrets table (safe to fail if already present)
    add_column_if_missing(pool, "secrets", "owner_group TEXT NOT NULL DEFAULT ''").await;

    // Add owner_group column to pipelines table (safe to fail if already present)
    add_column_if_missing(pool, "pipelines", "owner_group TEXT NOT NULL DEFAULT ''").await;

    // ── Multi-env governance: mandatory pipeline environments ──
    add_column_if_missing(pool, "pipelines", "environments TEXT NOT NULL DEFAULT '[]'").await;
    // End-of-build notify channels for form pipelines (YAML pipelines store
    // the same block in their file; the scanner mirrors it here).
    add_column_if_missing(pool, "pipelines", "notify_json TEXT NOT NULL DEFAULT '[]'").await;
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
    add_column_if_missing(pool, "pipelines", "run_counter INTEGER NOT NULL DEFAULT 0").await;
    add_column_if_missing(pool, "jobs", "run_number INTEGER NOT NULL DEFAULT 0").await;
    add_column_if_missing(pool, "jobs", "run_attempt INTEGER NOT NULL DEFAULT 1").await;
    backfill_job_run_numbers(pool).await?;
    // Re-run shares a pipeline's run_number across attempts (GHA style), so
    // uniqueness is (pipeline, run_number, attempt). `CREATE IF NOT EXISTS`
    // will not upgrade the legacy two-column index in place — rebuild it
    // whenever the stored definition predates run_attempt.
    let index_sql: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master \
         WHERE type = 'index' AND name = 'idx_jobs_pipeline_run_number'",
    )
    .fetch_optional(pool)
    .await?;
    let needs_rebuild = match index_sql.as_deref() {
        Some(sql) => !sql.contains("run_attempt"),
        None => true,
    };
    if needs_rebuild {
        sqlx::query("DROP INDEX IF EXISTS idx_jobs_pipeline_run_number")
            .execute(pool)
            .await?;
        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_pipeline_run_number \
             ON jobs(pipeline_id, run_number, run_attempt) WHERE run_number > 0",
        )
        .execute(pool)
        .await?;
    }

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
    add_column_if_missing(pool, "variables", "value TEXT NOT NULL DEFAULT ''").await;

    // Add host_names column to tasks — stores original host names before ID resolution
    add_column_if_missing(pool, "tasks", "host_names TEXT NOT NULL DEFAULT ''").await;

    // Add step_secrets column to tasks — per-step secret allowlist
    add_column_if_missing(pool, "tasks", "step_secrets TEXT NOT NULL DEFAULT ''").await;

    // Step-level env map JSON (`{"KEY":"${secrets.NAME}"}`) — GHA-style mapping
    add_column_if_missing(pool, "tasks", "step_env TEXT NOT NULL DEFAULT ''").await;

    // Private registry credentials secret name (docker / shell / SSH)
    add_column_if_missing(pool, "tasks", "registry_secret TEXT NOT NULL DEFAULT ''").await;

    // OSS config delivery + render (Config Block DSL v2 column names).
    add_column_if_missing(pool, "tasks", "config_source TEXT NOT NULL DEFAULT ''").await;
    // Legacy columns (pre-2b) — may still exist on upgraded DBs until cut-over below.
    add_column_if_missing(pool, "tasks", "config_path TEXT NOT NULL DEFAULT ''").await;
    add_column_if_missing(pool, "tasks", "env_render TEXT NOT NULL DEFAULT ''").await;
    add_column_if_missing(pool, "tasks", "config_dir TEXT NOT NULL DEFAULT ''").await;

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
    add_column_if_missing(pool, "hosts", "credential_data TEXT NOT NULL DEFAULT ''").await;

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
    add_column_if_missing(pool, "pipelines", "team_id TEXT NOT NULL DEFAULT ''").await;

    add_column_if_missing(pool, "secrets", "team_id TEXT NOT NULL DEFAULT ''").await;

    // Opt-in team auto-inject (default false = explicit bind/reference only)
    add_column_if_missing(pool, "secrets", "team_shared INTEGER NOT NULL DEFAULT 0").await;

    // Secret purpose: generic (env) | registry (docker login JSON)
    add_column_if_missing(pool, "secrets", "kind TEXT NOT NULL DEFAULT 'generic'").await;

    add_column_if_missing(pool, "hosts", "team_id TEXT").await;

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

    add_column_if_missing(
        pool,
        "pipelines",
        "concurrency_queue_timeout_seconds INTEGER NOT NULL DEFAULT 1800",
    )
    .await;

    add_column_if_missing(pool, "jobs", "queue_position INTEGER").await;

    // ── Plugin library (formerly "step marketplace", ADR step-marketplace §6.8): sources + pool ──
    // Plugin vocabulary: databases created before the rename carry the old
    // table names. Rename first — the CREATE statements below are no-ops for
    // renamed tables, and queries would otherwise hit "no such table".
    rename_table_if_present(pool, "step_sources", "plugin_sources").await;
    rename_table_if_present(pool, "step_pool", "plugin_pool").await;
    sqlx::query("DROP INDEX IF EXISTS idx_step_pool_lookup")
        .execute(pool)
        .await
        .ok();
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS plugin_sources (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            url TEXT NOT NULL,
            auto_enable INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            last_synced_at TEXT,
            last_sync_error TEXT,
            created_by TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    // Mirror endpoint for a source — consumption-side failover. The mirror is
    // tried before the canonical URL on every fetch; content is sha256-pinned
    // per file, so endpoint order affects availability only. Nullable:
    // legacy sources have no mirror.
    add_column_if_missing(pool, "plugin_sources", "mirror_url TEXT").await;

    // Anti-rollback watermark: the highest SIGNED catalog sequence this
    // source has accepted, the exact bytes it came with (same sequence but
    // different bytes = a forked index), and its issue time (display only).
    add_column_if_missing(pool, "plugin_sources", "last_index_sequence INTEGER").await;
    add_column_if_missing(pool, "plugin_sources", "last_index_sha256 TEXT").await;
    add_column_if_missing(pool, "plugin_sources", "last_index_issued_at TEXT").await;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS plugin_pool (
            namespace TEXT NOT NULL,
            plugin_id TEXT NOT NULL,
            version TEXT NOT NULL,
            sha256 TEXT NOT NULL,
            source_id TEXT NOT NULL REFERENCES plugin_sources(id) ON DELETE CASCADE,
            file_path TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 0,
            reviewed INTEGER NOT NULL DEFAULT 0,
            added_by TEXT,
            added_at TEXT NOT NULL DEFAULT (datetime('now')),
            requires_buz TEXT,
            buz_compatible INTEGER,
            compat_error TEXT,
            compat_checked_against TEXT,
            meta_json TEXT,
            UNIQUE(namespace, plugin_id, version)
        )",
    )
    .execute(pool)
    .await?;
    // Pre-rename databases carry the column as `step_id` (from the table's
    // own step_* days); SQLite's RENAME COLUMN also rewrites the UNIQUE
    // constraint and the lookup index that reference it. Run BEFORE the
    // index creation below so a fresh index sees the new name either way.
    rename_column_if_present(pool, "plugin_pool", "step_id", "plugin_id").await;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_plugin_pool_lookup ON plugin_pool (namespace, plugin_id, enabled)",
    )
    .execute(pool)
    .await?;
    // ── plugin_pool compatibility verdicts (computed at sync/refresh time) ──
    // buz_compatible is three-state: NULL = not checked yet, 0 = the entry's
    // requires_buz (or spec validity) fails against this manager build,
    // 1 = passes. compat_checked_against pins the manager version the verdict
    // was computed for; a version bump invalidates it.
    // Curation provenance: who last flipped a pool entry's enable state and
    // when. The audit log answers "what happened" (an event stream); these
    // columns answer "what is the state now" without a join. Sync never
    // writes them — they describe the curator's decision, not ingestion.
    add_column_if_missing(pool, "plugin_pool", "updated_by TEXT").await;
    add_column_if_missing(pool, "plugin_pool", "updated_at TEXT").await;

    add_column_if_missing(pool, "plugin_pool", "requires_buz TEXT").await;
    add_column_if_missing(pool, "plugin_pool", "buz_compatible INTEGER").await;
    add_column_if_missing(pool, "plugin_pool", "compat_error TEXT").await;
    add_column_if_missing(pool, "plugin_pool", "compat_checked_against TEXT").await;
    add_column_if_missing(pool, "plugin_pool", "meta_json TEXT").await;

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
    // Settings keys follow the plugin vocabulary. Old overrides are renamed,
    // otherwise an upgraded deployment silently loses its review gate and
    // sync timeout (the new-key lookup would find no row).
    for (old_key, new_key) in [
        ("step_review_required", "plugin_review_required"),
        ("step_reviewed_manifest", "plugin_reviewed_manifest"),
        ("step_index_local_dir", "plugin_index_local_dir"),
        ("step_sync_timeout", "plugin_sync_timeout"),
        (
            "step_official_source_dismissed",
            "plugin_official_source_dismissed",
        ),
    ] {
        // A new-format key wins over an old one (the upsert path always
        // writes new-format keys).
        sqlx::query("UPDATE platform_settings SET key = ? WHERE key = ?")
            .bind(new_key)
            .bind(old_key)
            .execute(pool)
            .await?;
    }

    // ── Official plugin source (factory-preseeded, auto-enabled) ──
    // The CF Pages mirror is the distribution endpoint chosen for
    // China-mainland reachability (§6.3); admins can delete/re-add the row
    // with an internal mirror URL if their network requires it.
    sqlx::query(
        "INSERT INTO plugin_sources (id, name, url, auto_enable, enabled, created_by) \
         SELECT 'official', 'Official buz-plugins', \
                'https://plugins.buz.schiplat.com/index.json', 1, 1, 'system' \
         WHERE NOT EXISTS (SELECT 1 FROM plugin_sources WHERE id = 'official') \
           AND NOT EXISTS ( \
               SELECT 1 FROM platform_settings \
               WHERE key = 'plugin_official_source_dismissed' AND value = 'true')",
    )
    .execute(pool)
    .await?;

    // ── Scheduler leader-lease table (multi-row: one lease per scheduler) ──
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS scheduler_lease (
            id INTEGER PRIMARY KEY,
            instance_id TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    // Legacy singleton schema carried `CHECK (id = 1)`; the GitHub auto-fetch
    // scheduler leases row 2 and would hit the CHECK on every cycle. Rebuild
    // once when the constraint is still present (row id=1 carries over).
    let lease_sql: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'scheduler_lease'",
    )
    .fetch_optional(pool)
    .await?;
    if lease_sql.is_some_and(|s| s.contains("CHECK (id = 1)")) {
        let mut tx = pool.begin().await?;
        sqlx::query(
            "CREATE TABLE scheduler_lease_new (
                id INTEGER PRIMARY KEY,
                instance_id TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT OR REPLACE INTO scheduler_lease_new (id, instance_id, expires_at, updated_at)
                SELECT id, instance_id, expires_at, updated_at FROM scheduler_lease",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query("DROP TABLE scheduler_lease")
            .execute(&mut *tx)
            .await?;
        sqlx::query("ALTER TABLE scheduler_lease_new RENAME TO scheduler_lease")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }

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

    add_column_if_missing(pool, "pipeline_git_hooks", "label TEXT NOT NULL DEFAULT ''").await;
    // Which normalized events this hook accepts; empty/default = push only,
    // preserving the pre-PR behavior for existing hooks.
    add_column_if_missing(
        pool,
        "pipeline_git_hooks",
        "event_filter TEXT NOT NULL DEFAULT 'push'",
    )
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
    add_column_if_missing(pool, "users", "is_super_admin INTEGER NOT NULL DEFAULT 0").await;

    // Platform-ops flag: sandbox host administration (ops API namespace).
    // Implies nothing else — admins keep full access; ops users are scoped to
    // the ops namespace and read-only elsewhere.
    add_column_if_missing(pool, "users", "is_ops INTEGER NOT NULL DEFAULT 0").await;

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

    // Token authority band (`read` / `ops`; `full` = legacy creator-equivalent
    // band, closed to new tokens). Pre-scope rows keep `full` so their
    // operations-plane behavior is unchanged; see rbac.rs for the plane policy.
    add_column_if_missing(pool, "api_tokens", "scope TEXT NOT NULL DEFAULT 'full'").await;

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
    add_column_if_missing(pool, "github_connections", "fetch_interval_minutes INTEGER").await;
    add_column_if_missing(pool, "github_connections", "last_fetch_at TEXT").await;

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
    add_column_if_missing(pool, "variable_github_binds", "override_value TEXT").await;
    add_column_if_missing(pool, "secret_github_binds", "override_value TEXT").await;

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
    add_column_if_missing(
        pool,
        "jobs",
        "environment_id TEXT REFERENCES environments(id) ON DELETE SET NULL",
    )
    .await;

    let _ = sqlx::query("CREATE INDEX IF NOT EXISTS idx_jobs_environment ON jobs(environment_id)")
        .execute(pool)
        .await;

    // Trigger user id for self-approve checks (NULL for webhook/cron/git).
    add_column_if_missing(pool, "jobs", "trigger_user_id TEXT").await;

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
    add_column_if_missing(pool, "job_approvals", "username TEXT NOT NULL DEFAULT ''").await;

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

    Ok(())
}

pub async fn migrate_step_resolutions(pool: &SqlitePool) -> anyhow::Result<()> {
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

    add_column_if_missing(pool, "tasks", "config_key_path TEXT NOT NULL DEFAULT ''").await;
    add_column_if_missing(pool, "tasks", "config_dest TEXT NOT NULL DEFAULT ''").await;
    add_column_if_missing(pool, "tasks", "config_render TEXT NOT NULL DEFAULT ''").await;
    // `needs:` DAG edges (JSON array of step names); empty = sequential order.
    add_column_if_missing(pool, "tasks", "needs_json TEXT NOT NULL DEFAULT '[]'").await;
    // Step orders whose log output was truncated by the Runner's per-step
    // window. Job metadata, not log data: it must outlive job_log_retention.
    add_column_if_missing(pool, "jobs", "truncated_steps TEXT NOT NULL DEFAULT '[]'").await;

    let _ =
        sqlx::query("ALTER TABLE tasks ADD COLUMN hosts_strategy TEXT NOT NULL DEFAULT 'chain'")
            .execute(pool)
            .await;
    add_column_if_missing(pool, "tasks", "hosts_max_parallel INTEGER").await;

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
pub async fn migrate_pipeline_git_hooks_multi(pool: &SqlitePool) -> anyhow::Result<()> {
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

/// Assign run numbers to jobs that predate the feature (run_number = 0), in
/// created_at order per pipeline, then move each pipeline's allocation cursor
/// past its history. Idempotent: numbered rows are left untouched.
pub async fn backfill_job_run_numbers(pool: &SqlitePool) -> anyhow::Result<()> {
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
