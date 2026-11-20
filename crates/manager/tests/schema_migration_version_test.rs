//! The migration runner is versioned: a version recorded in
//! `schema_migrations` must not be replayed, and a fresh database must get the
//! baseline applied exactly once.
//!
//! Before versioning, every startup replayed the full baseline. This test
//! pins the gate both ways: the baseline runs on a virgin database, and is
//! skipped (observably: no tables created) when its version is already
//! recorded.

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

const SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";

async fn fresh_pool() -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap()
}

async fn table_exists(pool: &SqlitePool, name: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap_or(0)
        != 0
}

async fn recorded_versions(pool: &SqlitePool) -> Vec<i64> {
    sqlx::query_scalar("SELECT version FROM schema_migrations ORDER BY version")
        .fetch_all(pool)
        .await
        .unwrap_or_default()
}

/// A virgin database gets the baseline and records version 1.
#[tokio::test]
async fn fresh_database_applies_baseline_and_records_it() {
    let pool = fresh_pool().await;
    manager::db::run_migrations(&pool, SECRET).await.unwrap();

    assert!(table_exists(&pool, "pipelines").await, "baseline must run");
    let indexed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_pipelines_yaml_path'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(indexed, 1, "definition-name uniqueness must be enforced");
    assert_eq!(recorded_versions(&pool).await, vec![1, 2, 3, 4, 5, 6, 7, 8]);
}

/// A recorded version is not replayed: seed the version table on an otherwise
/// empty database — nothing may be created, and no error may surface from a
/// migration running against tables that do not exist.
#[tokio::test]
async fn recorded_version_is_not_replayed() {
    let pool = fresh_pool().await;
    sqlx::query(
        "CREATE TABLE schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO schema_migrations (version) VALUES (1), (2), (3), (4), (5), (6), (7), (8)",
    )
    .execute(&pool)
    .await
    .unwrap();

    manager::db::run_migrations(&pool, SECRET).await.unwrap();

    assert!(
        !table_exists(&pool, "pipelines").await,
        "an applied baseline must not be replayed"
    );
    assert_eq!(recorded_versions(&pool).await, vec![1, 2, 3, 4, 5, 6, 7, 8]);
}

/// `seq` replaces implicit `rowid` ordering: inserts get a monotonically
/// increasing number, and pre-existing rows keep their historical order via
/// the rowid backfill.
#[tokio::test]
async fn seq_is_assigned_on_insert_and_ordered() {
    let pool = fresh_pool().await;
    manager::db::run_migrations(&pool, SECRET).await.unwrap();

    // jobs.pipeline_id references pipelines(id); seed one shell row.
    sqlx::query("INSERT INTO pipelines (id, name, trigger_type, environments, team_id, input_defs) VALUES ('pipe', 'pipe', 'manual', '[]', 'team-default', '[]')")
        .execute(&pool)
        .await
        .unwrap();

    async fn insert_job(pool: &SqlitePool, id: &str) {
        sqlx::query(
            "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot, created_at, updated_at)
             VALUES (?, 'pipe', 'pending', 'test', '{}', datetime('now'), datetime('now'))",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }
    insert_job(&pool, "job-a").await;
    insert_job(&pool, "job-b").await;
    insert_job(&pool, "job-c").await;

    let ordered: Vec<(String, i64)> = sqlx::query_as("SELECT id, seq FROM jobs ORDER BY seq")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        ordered,
        vec![
            ("job-a".into(), 1),
            ("job-b".into(), 2),
            ("job-c".into(), 3),
        ],
        "insertion order must be explicit and monotonic"
    );

    // Explicitly numbered inserts are respected, not overwritten.
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot, seq, created_at, updated_at)
         VALUES ('job-x', 'pipe', 'pending', 'test', '{}', 99, datetime('now'), datetime('now'))",
    )
    .execute(&pool)
    .await
    .unwrap();
    let seq: i64 = sqlx::query_scalar("SELECT seq FROM jobs WHERE id = 'job-x'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(seq, 99);

    // Next automatic insert continues past the explicit maximum.
    insert_job(&pool, "job-d").await;
    let seq: i64 = sqlx::query_scalar("SELECT seq FROM jobs WHERE id = 'job-d'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(seq, 100);
}

/// Running the runner repeatedly is a no-op after the first application.
#[tokio::test]
async fn second_run_changes_nothing() {
    let pool = fresh_pool().await;
    manager::db::run_migrations(&pool, SECRET).await.unwrap();
    manager::db::run_migrations(&pool, SECRET).await.unwrap();
    manager::db::run_migrations(&pool, SECRET).await.unwrap();

    assert_eq!(recorded_versions(&pool).await, vec![1, 2, 3, 4, 5, 6, 7, 8]);
}

/// v8 upgrade path: a v7 database with `owner_group` slugs and the global
/// name constraints is backfilled, rebuilt, and re-keyed without losing rows.
#[tokio::test]
async fn v8_backfills_owner_group_and_normalizes_bindings() {
    let pool = fresh_pool().await;
    for sql in [
        "CREATE TABLE schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        "INSERT INTO schema_migrations (version) VALUES (1), (2), (3), (4), (5), (6), (7)",
        "CREATE TABLE teams (id TEXT PRIMARY KEY, name TEXT NOT NULL, slug TEXT NOT NULL UNIQUE)",
        "CREATE TABLE pipelines (
            id TEXT PRIMARY KEY, name TEXT NOT NULL, team_id TEXT NOT NULL DEFAULT '',
            owner_group TEXT NOT NULL DEFAULT ''
        )",
        "CREATE TABLE secrets (
            id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, value TEXT NOT NULL DEFAULT '',
            owner_group TEXT NOT NULL DEFAULT '', team_id TEXT NOT NULL DEFAULT '',
            team_shared INTEGER NOT NULL DEFAULT 0, kind TEXT NOT NULL DEFAULT 'generic',
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        "CREATE TABLE pipeline_secrets (
            pipeline_id TEXT NOT NULL REFERENCES pipelines(id) ON DELETE CASCADE,
            secret_id TEXT NOT NULL REFERENCES secrets(id) ON DELETE CASCADE,
            UNIQUE(pipeline_id, secret_id)
        )",
        "CREATE TABLE hosts (
            id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, host TEXT NOT NULL,
            port INTEGER NOT NULL DEFAULT 22, username TEXT NOT NULL,
            credential_type TEXT NOT NULL DEFAULT 'vault',
            credential_data TEXT NOT NULL DEFAULT '',
            credential_passphrase TEXT NOT NULL DEFAULT '',
            labels TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            team_id TEXT
        )",
        "INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Alpha', 'alpha')",
        "INSERT INTO pipelines (id, name, owner_group) VALUES ('p1', 'alpha-pipe', 'alpha')",
        "INSERT INTO secrets (id, name, value, owner_group, team_shared)
         VALUES ('s1', 'DEPLOY_KEY', 'x', 'alpha', 1)",
        "INSERT INTO pipelines (id, name, owner_group) VALUES ('p2', 'legacy-pipe', 'legacy')",
        "INSERT INTO secrets (id, name, value, owner_group)
         VALUES ('s2', 'LEGACY_KEY', 'x', 'legacy')",
        "INSERT INTO pipeline_secrets (pipeline_id, secret_id) VALUES ('p1', 's1')",
    ] {
        sqlx::query(sql)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    manager::db::run_migrations(&pool, SECRET).await.unwrap();

    let pipeline_team: String = sqlx::query_scalar("SELECT team_id FROM pipelines WHERE id = 'p1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pipeline_team, "team-1");
    let (secret_team, shared): (String, i64) =
        sqlx::query_as("SELECT team_id, share_with_team FROM secrets WHERE id = 's1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(secret_team, "team-1");
    assert_eq!(shared, 1, "share_with_team keeps the old opt-in value");

    // A Phase 1 slug without a teams row gets one materialised, and both rows
    // point at it instead of falling into `default`.
    let legacy_team: String = sqlx::query_scalar("SELECT id FROM teams WHERE slug = 'legacy'")
        .fetch_one(&pool)
        .await
        .unwrap();
    for sql in [
        "SELECT team_id FROM pipelines WHERE id = 'p2'",
        "SELECT team_id FROM secrets WHERE id = 's2'",
    ] {
        let team: String = sqlx::query_scalar(sql).fetch_one(&pool).await.unwrap();
        assert_eq!(team, legacy_team, "{sql}");
    }

    // Rebuilding `secrets` must not cascade-delete child rows.
    let bindings: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pipeline_secrets WHERE secret_id = 's1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(bindings, 1, "pipeline_secrets survived the table rebuild");

    for legacy in [
        "SELECT owner_group FROM pipelines LIMIT 0",
        "SELECT owner_group FROM secrets LIMIT 0",
        "SELECT team_shared FROM secrets LIMIT 0",
    ] {
        assert!(
            sqlx::query(legacy).execute(&pool).await.is_err(),
            "legacy column still present: {legacy}"
        );
    }
    assert_eq!(recorded_versions(&pool).await, vec![1, 2, 3, 4, 5, 6, 7, 8]);
}
