//! Concurrency `queue_timeout` enforcement: queued jobs waiting past the
//! snapshot-frozen timeout are cancelled with a snapshot annotation; fresh
//! jobs, the default window, and non-queued statuses are untouched. Also
//! pins the cron scheduler's lease row: it must be a third dedicated row,
//! independent of the dispatch and GitHub-fetch rows.

mod common;

use manager::store::exec;

use manager::services::scheduler::{
    expire_queue_timeouts, try_acquire_lease, CRON_TRIGGER_LEASE_ID, JOB_DISPATCH_LEASE_ID,
};

async fn setup() -> exec::DbPool {
    let pool = common::test_db(4).await;
    exec::execute(
        &pool,
        "INSERT INTO pipelines (id, name) VALUES ('p1', 'demo')",
        &[],
    )
    .await
    .expect("seed pipeline");
    pool
}

/// Insert a queued job with the given frozen snapshot, entered `age` ago.
async fn seed_queued_job(pool: &exec::DbPool, id: &str, snapshot: &str, age: &str) {
    exec::execute(pool,
    "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot, debug_unredact, created_at, updated_at)
         VALUES (?, 'p1', 'queued', 'test', ?, 0, datetime('now', ?), datetime('now', ?))",
    &[id.into(), snapshot.into(), age.into(), age.into()],
    )
    .await
    .expect("seed queued job");
}

async fn job_status(pool: &exec::DbPool, id: &str) -> (String, Option<String>) {
    let row = exec::fetch_optional(
        pool,
        "SELECT status, end_time FROM jobs WHERE id = ?",
        &[id.into()],
    )
    .await
    .expect("load job")
    .expect("row");
    (row.text(0), row.opt_text(1))
}

async fn snapshot(pool: &exec::DbPool, id: &str) -> serde_json::Value {
    let raw: String =
        exec::fetch_optional(pool, "SELECT snapshot FROM jobs WHERE id = ?", &[id.into()])
            .await
            .unwrap()
            .expect("load snapshot")
            .text(0);
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

    expire_queue_timeouts(&pool.clone())
        .await
        .expect("expiry runs");

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

    expire_queue_timeouts(&pool.clone())
        .await
        .expect("expiry runs");
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

    expire_queue_timeouts(&pool.clone())
        .await
        .expect("expiry runs");
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
        exec::execute(
            &pool,
            &format!(
            "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot, debug_unredact, created_at, updated_at)
             VALUES ('{id}', 'p1', '{status}', 'test', '{{}}', 0, datetime('now', '-2 hours'), datetime('now', '-2 hours'))"
        ),
            &[],
        )
        .await
        .expect("seed job");
    }

    expire_queue_timeouts(&pool.clone())
        .await
        .expect("expiry runs");
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
        try_acquire_lease(&pool.clone(), JOB_DISPATCH_LEASE_ID, a)
            .await
            .expect("dispatch lease"),
        "dispatch lease row is free"
    );
    assert!(
        try_acquire_lease(&pool.clone(), CRON_TRIGGER_LEASE_ID, b)
            .await
            .expect("cron lease"),
        "cron lease row is independent of the dispatch row"
    );

    // Contention: a second instance cannot take a held cron lease...
    assert!(
        !try_acquire_lease(&pool.clone(), CRON_TRIGGER_LEASE_ID, a)
            .await
            .expect("contended cron lease"),
        "held cron lease blocks another instance"
    );
    // ...nor may the cron holder touch the dispatch row.
    assert!(
        !try_acquire_lease(&pool.clone(), JOB_DISPATCH_LEASE_ID, b)
            .await
            .expect("contended dispatch lease"),
        "held dispatch lease blocks the cron instance"
    );
}
