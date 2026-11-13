//! Terminal-job log retention: logs of terminal jobs past the retention
//! window are purged (job rows kept, `logs_purged` marker set, WS sink
//! dropped); fresh jobs, non-terminal jobs, and debug jobs are untouched;
//! `job_log_retention_days = 0` disables the sweep.

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use manager::db::run_migrations;
use manager::services::job_log_retention::purge_expired_job_logs;
use manager::services::log_broadcast::LogBroadcaster;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";

async fn setup() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");
    sqlx::query("INSERT INTO pipelines (id, name) VALUES ('p1', 'demo')")
        .execute(&pool)
        .await
        .expect("seed pipeline");
    pool
}

/// Override the retention setting (rows: platform_settings(key, value)).
async fn set_retention_days(pool: &SqlitePool, days: &str) {
    sqlx::query("INSERT OR REPLACE INTO platform_settings (key, value) VALUES ('job_log_retention_days', ?)")
        .bind(days)
        .execute(pool)
        .await
        .expect("set retention setting");
}

/// Insert a terminal job created `age` ago, with two log rows.
async fn seed_job(pool: &SqlitePool, id: &str, status: &str, age: &str, debug: bool) {
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot, debug_unredact, created_at, updated_at)
         VALUES (?, 'p1', ?, 'test', '{}', ?, datetime('now', ?), datetime('now', ?))",
    )
    .bind(id)
    .bind(status)
    .bind(if debug { 1 } else { 0 })
    .bind(age)
    .bind(age)
    .execute(pool)
    .await
    .expect("seed job");
    for chunk in 0..2 {
        sqlx::query(
            "INSERT INTO job_logs (id, job_id, chunk_index, log_content) VALUES (?, ?, ?, 'data')",
        )
        .bind(format!("{id}-log-{chunk}"))
        .bind(id)
        .bind(chunk)
        .execute(pool)
        .await
        .expect("seed log");
    }
}

async fn log_count(pool: &SqlitePool, job_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM job_logs WHERE job_id = ?")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("count logs")
}

async fn purged_flag(pool: &SqlitePool, job_id: &str) -> i64 {
    sqlx::query_scalar("SELECT logs_purged FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("load flag")
}

#[tokio::test]
async fn purges_expired_terminal_jobs_only() {
    let pool = setup().await;
    let broadcaster = LogBroadcaster::new();
    // Default 30 days: 40-day-old terminal jobs expire, 10-day-old and
    // non-terminal rows do not. WS replay sinks are dropped for the purged.
    seed_job(&pool, "old-failed", "failed", "-40 days", false).await;
    seed_job(&pool, "old-success", "success", "-40 days", false).await;
    seed_job(&pool, "young-failed", "failed", "-10 days", false).await;
    seed_job(&pool, "old-running", "running", "-40 days", false).await;
    seed_job(&pool, "old-debug", "failed", "-40 days", true).await;
    broadcaster.broadcast("old-failed", 0, None, "history");

    let purged = purge_expired_job_logs(
        &manager::store::exec::DbPool::from_sqlite(&pool),
        &broadcaster,
    )
    .await
    .expect("purge runs");
    assert_eq!(purged, 2, "two expired terminal jobs purged");

    assert_eq!(log_count(&pool, "old-failed").await, 0);
    assert_eq!(purged_flag(&pool, "old-failed").await, 1);
    assert!(
        broadcaster.replay_history("old-failed").is_empty(),
        "WS sink dropped"
    );
    assert_eq!(log_count(&pool, "old-success").await, 0);
    assert_eq!(purged_flag(&pool, "old-success").await, 1);

    assert_eq!(
        log_count(&pool, "young-failed").await,
        2,
        "within window kept"
    );
    assert_eq!(purged_flag(&pool, "young-failed").await, 0);
    assert_eq!(
        log_count(&pool, "old-running").await,
        2,
        "running jobs never purged"
    );
    assert_eq!(
        log_count(&pool, "old-debug").await,
        2,
        "debug jobs belong to the debug cleaner"
    );
}

#[tokio::test]
async fn idempotent_after_marker_set() {
    let pool = setup().await;
    let broadcaster = LogBroadcaster::new();
    seed_job(&pool, "old", "failed", "-40 days", false).await;

    assert_eq!(
        purge_expired_job_logs(
            &manager::store::exec::DbPool::from_sqlite(&pool),
            &broadcaster
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        purge_expired_job_logs(
            &manager::store::exec::DbPool::from_sqlite(&pool),
            &broadcaster
        )
        .await
        .unwrap(),
        0,
        "marker prevents rescan"
    );
    assert_eq!(log_count(&pool, "old").await, 0);
}

#[tokio::test]
async fn setting_zero_disables_and_override_applies() {
    let pool = setup().await;
    let broadcaster = LogBroadcaster::new();
    seed_job(&pool, "old", "failed", "-40 days", false).await;

    // 0 = keep forever.
    set_retention_days(&pool, "0").await;
    assert_eq!(
        purge_expired_job_logs(
            &manager::store::exec::DbPool::from_sqlite(&pool),
            &broadcaster
        )
        .await
        .unwrap(),
        0
    );
    assert_eq!(log_count(&pool, "old").await, 2);

    // Override to 1 day: the 40-day-old job now expires.
    set_retention_days(&pool, "1").await;
    assert_eq!(
        purge_expired_job_logs(
            &manager::store::exec::DbPool::from_sqlite(&pool),
            &broadcaster
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(log_count(&pool, "old").await, 0);
}
