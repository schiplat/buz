//! Concurrency `queue_timeout` enforcement: queued jobs waiting past the
//! snapshot-frozen timeout are cancelled with a snapshot annotation; fresh
//! jobs, the default window, and non-queued statuses are untouched. Also
//! pins the cron scheduler's lease row: it must be a third dedicated row,
//! independent of the dispatch and GitHub-fetch rows.

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use manager::db::run_migrations;
use manager::services::scheduler::{
    expire_queue_timeouts, try_acquire_lease, CRON_TRIGGER_LEASE_ID, JOB_DISPATCH_LEASE_ID,
};

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

/// Insert a queued job with the given frozen snapshot, entered `age` ago.
async fn seed_queued_job(pool: &SqlitePool, id: &str, snapshot: &str, age: &str) {
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot, debug_unredact, created_at, updated_at)
         VALUES (?, 'p1', 'queued', 'test', ?, 0, datetime('now', ?), datetime('now', ?))",
    )
    .bind(id)
    .bind(snapshot)
    .bind(age)
    .bind(age)
    .execute(pool)
    .await
    .expect("seed queued job");
}

async fn job_status(pool: &SqlitePool, id: &str) -> (String, Option<String>) {
    let row: (String, Option<String>) =
        sqlx::query_as("SELECT status, end_time FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("load job");
    row
}

async fn snapshot(pool: &SqlitePool, id: &str) -> serde_json::Value {
    let raw: String = sqlx::query_scalar("SELECT snapshot FROM jobs WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("load snapshot");
    serde_json::from_str(&raw).unwrap_or(serde_json::json!({}))
}

#[tokio::test]
async fn expired_queued_job_is_cancelled_with_snapshot_annotation() {
    let pool = setup().await;
    // Snapshot-frozen timeout 60s; the job has queued 2 minutes ago.
    seed_queued_job(
        &pool,
        "expired",
        r#"{"concurrency_queue_timeout_seconds": 60}"#,
        "-2 minutes",
    )
    .await;

    expire_queue_timeouts(&pool).await.expect("expiry runs");

    let (status, end_time) = job_status(&pool, "expired").await;
    assert_eq!(status, "cancelled", "expired queued job cancelled");
    assert!(end_time.is_some(), "end_time recorded");
    assert_eq!(
        snapshot(&pool, "expired").await["concurrency_queue_timed_out"],
        serde_json::json!(true),
        "cancel-reason annotation written"
    );
}

#[tokio::test]
async fn fresh_queued_job_survives() {
    let pool = setup().await;
    seed_queued_job(
        &pool,
        "fresh",
        r#"{"concurrency_queue_timeout_seconds": 1800}"#,
        "-30 seconds",
    )
    .await;

    expire_queue_timeouts(&pool).await.expect("expiry runs");
    assert_eq!(job_status(&pool, "fresh").await.0, "queued");
    assert!(
        snapshot(&pool, "fresh")
            .await
            .get("concurrency_queue_timed_out")
            .is_none(),
        "no annotation on surviving jobs"
    );
}

#[tokio::test]
async fn default_window_applies_when_snapshot_has_no_timeout() {
    let pool = setup().await;
    // Legacy/empty snapshot → the 1800s save-time default; 31 minutes past.
    seed_queued_job(&pool, "legacy", "{}", "-31 minutes").await;

    expire_queue_timeouts(&pool).await.expect("expiry runs");
    assert_eq!(
        job_status(&pool, "legacy").await.0,
        "cancelled",
        "empty snapshot falls back to the default window"
    );
}

#[tokio::test]
async fn non_queued_statuses_are_never_touched() {
    let pool = setup().await;
    for (id, status) in [
        ("old-pending", "pending"),
        ("old-running", "running"),
        ("old-waiting", "waiting_approval"),
    ] {
        sqlx::query(&format!(
            "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot, debug_unredact, created_at, updated_at)
             VALUES ('{id}', 'p1', '{status}', 'test', '{{}}', 0, datetime('now', '-2 hours'), datetime('now', '-2 hours'))"
        ))
        .execute(&pool)
        .await
        .expect("seed job");
    }

    expire_queue_timeouts(&pool).await.expect("expiry runs");
    for (id, status) in [
        ("old-pending", "pending"),
        ("old-running", "running"),
        ("old-waiting", "waiting_approval"),
    ] {
        assert_eq!(
            job_status(&pool, id).await.0,
            status,
            "{id} must keep its status"
        );
    }
}

#[tokio::test]
async fn cron_lease_row_is_independent_of_dispatch_row() {
    let pool = setup().await;
    let a = "instance-a";
    let b = "instance-b";

    // Two schedulers hold their own rows simultaneously.
    assert!(
        try_acquire_lease(&pool, JOB_DISPATCH_LEASE_ID, a)
            .await
            .expect("dispatch lease"),
        "dispatch lease row is free"
    );
    assert!(
        try_acquire_lease(&pool, CRON_TRIGGER_LEASE_ID, b)
            .await
            .expect("cron lease"),
        "cron lease row is independent of the dispatch row"
    );

    // Contention: a second instance cannot take a held cron lease...
    assert!(
        !try_acquire_lease(&pool, CRON_TRIGGER_LEASE_ID, a)
            .await
            .expect("contended cron lease"),
        "held cron lease blocks another instance"
    );
    // ...nor may the cron holder touch the dispatch row.
    assert!(
        !try_acquire_lease(&pool, JOB_DISPATCH_LEASE_ID, b)
            .await
            .expect("contended dispatch lease"),
        "held dispatch lease blocks the cron instance"
    );
}
