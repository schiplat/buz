//! The scheduler lease table must accept one row per background scheduler:
//! job dispatch leases row 1, GitHub auto-fetch leases row 2. Legacy
//! databases carried a single-row `CHECK (id = 1)` schema that made the
//! second lease fail on every cycle (v2.1.2 regression); `run_migrations`
//! rebuilds such tables once.

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

async fn fresh_pool() -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap()
}

async fn insert_lease(pool: &SqlitePool, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO scheduler_lease (id, instance_id, expires_at) VALUES (?, 'test-instance', datetime('now', '+60 seconds'))")
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Legacy database: table exists with the singleton CHECK and one row —
/// run_migrations must rebuild it so both lease rows fit.
#[tokio::test]
async fn legacy_single_row_lease_schema_is_rebuilt() {
    let pool = fresh_pool().await;
    sqlx::query(
        "CREATE TABLE scheduler_lease (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            instance_id TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    insert_lease(&pool, 1).await.unwrap();

    manager::db::run_migrations(&pool, "test-secret-0123456789-abcdefghijklmnop")
        .await
        .unwrap();

    // Row 1 (job dispatch lease) survives the rebuild...
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, instance_id FROM scheduler_lease ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows, vec![(1, "test-instance".to_string())]);
    // ...and row 2 (GitHub auto-fetch lease) must now be insertable.
    insert_lease(&pool, 2).await.unwrap();
}

/// Fresh database: migrations create the multi-row schema directly.
#[tokio::test]
async fn fresh_lease_schema_accepts_both_leases() {
    let pool = fresh_pool().await;
    manager::db::run_migrations(&pool, "test-secret-0123456789-abcdefghijklmnop")
        .await
        .unwrap();
    insert_lease(&pool, 1).await.unwrap();
    insert_lease(&pool, 2).await.unwrap();
}
