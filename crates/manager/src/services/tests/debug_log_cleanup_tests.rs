//! Tests for debug log retention: only debug jobs older than the retention
//! window get their logs purged and their flag reset; fresh debug jobs and
//! non-debug jobs are untouched. Included via `services/debug_log_cleanup.rs`
//! under `#[cfg(test)]`.

use sqlx::sqlite::SqlitePoolOptions;

use crate::db::run_migrations;
use crate::services::debug_log_cleanup::purge_expired_debug_logs;
use crate::services::log_broadcast::LogBroadcaster;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";

async fn setup() -> sqlx::SqlitePool {
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

/// Insert a debug job whose `created_at` is 25h in the past (expired).
async fn seed_expired_debug_job(pool: &sqlx::SqlitePool, id: &str) {
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot, debug_unredact, created_at, updated_at)
         VALUES (?, 'p1', 'failed', 'test', '{}', 1, datetime('now', '-25 hours'), datetime('now', '-25 hours'))",
    )
    .bind(id)
    .execute(pool)
    .await
    .expect("seed expired debug job");
    sqlx::query(
        "INSERT INTO job_logs (id, job_id, chunk_index, log_content) VALUES (?, ?, 0, 'plaintext')",
    )
    .bind(format!("{id}-log"))
    .bind(id)
    .execute(pool)
    .await
    .expect("seed log");
}

/// Insert a debug job created now (within retention) or a non-debug job.
async fn seed_job(pool: &sqlx::SqlitePool, id: &str, debug: bool) {
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot, debug_unredact)
         VALUES (?, 'p1', 'failed', 'test', '{}', ?)",
    )
    .bind(id)
    .bind(if debug { 1 } else { 0 })
    .execute(pool)
    .await
    .expect("seed job");
    sqlx::query(
        "INSERT INTO job_logs (id, job_id, chunk_index, log_content) VALUES (?, ?, 0, 'plaintext')",
    )
    .bind(format!("{id}-log"))
    .bind(id)
    .execute(pool)
    .await
    .expect("seed log");
}

async fn log_count(pool: &sqlx::SqlitePool, job_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM job_logs WHERE job_id = ?")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("count logs")
}

async fn debug_flag(pool: &sqlx::SqlitePool, job_id: &str) -> i64 {
    sqlx::query_scalar("SELECT debug_unredact FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("load flag")
}

#[tokio::test]
async fn purges_only_expired_debug_jobs() {
    let pool = setup().await;
    let broadcaster = LogBroadcaster::new();
    seed_expired_debug_job(&pool, "old-debug").await;
    seed_job(&pool, "fresh-debug", true).await;
    seed_job(&pool, "old-normal", false).await;

    // Simulate a debug job whose unredacted chunks were broadcast before any
    // WS client connected (the sink retains them in memory).
    broadcaster.broadcast("old-debug", 0, None, "plaintext-secret");

    let purged = purge_expired_debug_logs(&pool, &broadcaster)
        .await
        .expect("purge runs");
    assert_eq!(purged, 1, "only the expired debug job is purged");

    assert_eq!(
        log_count(&pool, "old-debug").await,
        0,
        "expired debug logs removed"
    );
    assert_eq!(
        debug_flag(&pool, "old-debug").await,
        0,
        "expired debug flag reset"
    );
    assert!(
        broadcaster.replay_history("old-debug").is_empty(),
        "in-memory replay sink for the expired debug job is dropped"
    );

    assert_eq!(
        log_count(&pool, "fresh-debug").await,
        1,
        "fresh debug logs kept"
    );
    assert_eq!(
        debug_flag(&pool, "fresh-debug").await,
        1,
        "fresh debug flag kept"
    );

    assert_eq!(
        log_count(&pool, "old-normal").await,
        1,
        "non-debug logs never touched"
    );
    assert_eq!(debug_flag(&pool, "old-normal").await, 0);
}

#[tokio::test]
async fn noop_when_no_expired_debug_jobs() {
    let pool = setup().await;
    let broadcaster = LogBroadcaster::new();
    seed_job(&pool, "fresh-debug", true).await;

    let purged = purge_expired_debug_logs(&pool, &broadcaster)
        .await
        .expect("purge runs");
    assert_eq!(purged, 0);
    assert_eq!(debug_flag(&pool, "fresh-debug").await, 1);
    // Fresh debug job keeps its in-memory sink (admin may still stream it live).
    assert!(broadcaster.replay_history("fresh-debug").is_empty());
}

#[tokio::test]
async fn expired_non_debug_jobs_are_not_touched() {
    let pool = setup().await;
    let broadcaster = LogBroadcaster::new();
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot, debug_unredact, created_at, updated_at)
         VALUES ('old-normal', 'p1', 'failed', 'test', '{}', 0, datetime('now', '-25 hours'), datetime('now', '-25 hours'))",
    )
    .execute(&pool)
    .await
    .expect("seed old non-debug job");
    sqlx::query("INSERT INTO job_logs (id, job_id, chunk_index, log_content) VALUES ('old-normal-log', 'old-normal', 0, 'data')")
        .execute(&pool)
        .await
        .expect("seed log");

    let purged = purge_expired_debug_logs(&pool, &broadcaster)
        .await
        .expect("purge runs");
    assert_eq!(purged, 0);
    assert_eq!(
        log_count(&pool, "old-normal").await,
        1,
        "old non-debug logs kept"
    );
}
