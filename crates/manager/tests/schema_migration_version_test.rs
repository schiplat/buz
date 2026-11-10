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
    assert_eq!(recorded_versions(&pool).await, vec![1, 2]);
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
    sqlx::query("INSERT INTO schema_migrations (version) VALUES (1), (2)")
        .execute(&pool)
        .await
        .unwrap();

    manager::db::run_migrations(&pool, SECRET).await.unwrap();

    assert!(
        !table_exists(&pool, "pipelines").await,
        "an applied baseline must not be replayed"
    );
    assert_eq!(recorded_versions(&pool).await, vec![1, 2]);
}

/// Running the runner repeatedly is a no-op after the first application.
#[tokio::test]
async fn second_run_changes_nothing() {
    let pool = fresh_pool().await;
    manager::db::run_migrations(&pool, SECRET).await.unwrap();
    manager::db::run_migrations(&pool, SECRET).await.unwrap();
    manager::db::run_migrations(&pool, SECRET).await.unwrap();

    assert_eq!(recorded_versions(&pool).await, vec![1, 2]);
}
