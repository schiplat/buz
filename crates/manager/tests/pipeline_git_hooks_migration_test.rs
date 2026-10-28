use manager::db::migrate_pipeline_git_hooks_multi;
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
