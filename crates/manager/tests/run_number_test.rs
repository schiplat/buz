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
    manager::db::run_migrations(&pool, "test-secret-0123456789abcdef")
        .await
        .unwrap();
    seed_pipeline(&pool, "p1").await;
    // Inserted newest-first on purpose: backfill must order by created_at.
    seed_legacy_job(&pool, "j-new", "p1", "2026-01-03 00:00:00").await;
    seed_legacy_job(&pool, "j-mid", "p1", "2026-01-02 00:00:00").await;
    seed_legacy_job(&pool, "j-old", "p1", "2026-01-01 00:00:00").await;
    // Simulate the upgrade moment: these jobs existed before the feature.
    manager::db::backfill_job_run_numbers(&pool).await.unwrap();

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
        manager::db::allocate_run_number(&mut conn, "p1")
            .await
            .unwrap(),
        4
    );
}

#[tokio::test]
async fn concurrent_allocations_are_unique_and_dense() {
    let pool = pool().await;
    manager::db::run_migrations(&pool, "test-secret-0123456789abcdef")
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
            manager::db::allocate_run_number(&mut conn, "p1")
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
        manager::db::allocate_run_number(&mut conn, "p2")
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn partial_unique_index_rejects_duplicate_numbers() {
    let pool = pool().await;
    manager::db::run_migrations(&pool, "test-secret-0123456789abcdef")
        .await
        .unwrap();
    seed_pipeline(&pool, "p1").await;
    seed_legacy_job(&pool, "j1", "p1", "2026-01-01 00:00:00").await;
    manager::db::backfill_job_run_numbers(&pool).await.unwrap();
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
