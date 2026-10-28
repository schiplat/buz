use manager::db::migrate_step_resolutions;
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
