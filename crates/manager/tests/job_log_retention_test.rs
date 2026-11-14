//! Terminal-job log retention: logs of terminal jobs past the retention
//! window are purged (job rows kept, `logs_purged` marker set, WS sink
//! dropped); fresh jobs, non-terminal jobs, and debug jobs are untouched;
//! `job_log_retention_days = 0` disables the sweep.

mod common;

use manager::store::exec;

use manager::services::job_log_retention::purge_expired_job_logs;
use manager::services::log_broadcast::LogBroadcaster;

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

/// Override the retention setting (rows: platform_settings(key, value)).
async fn set_retention_days(pool: &exec::DbPool, days: &str) {
    exec::execute(
        pool,
        "INSERT INTO platform_settings (key, value) VALUES ('job_log_retention_days', ?)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        &[days.into()],
    )
    .await
    .expect("set retention setting");
}

/// Insert a terminal job created `age` ago, with two log rows.
async fn seed_job(pool: &exec::DbPool, id: &str, status: &str, age: &str, debug: bool) {
    exec::execute(pool,
    "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot, debug_unredact, created_at, updated_at)
         VALUES (?, 'p1', ?, 'test', '{}', ?, datetime('now', ?), datetime('now', ?))",
    &[id.into(), status.into(), if debug { 1 } else { 0 }.into(), age.into(), age.into()],
    )
    .await
    .expect("seed job");
    for chunk in 0..2 {
        exec::execute(
            pool,
            "INSERT INTO job_logs (id, job_id, chunk_index, log_content) VALUES (?, ?, ?, 'data')",
            &[format!("{id}-log-{chunk}").into(), id.into(), chunk.into()],
        )
        .await
        .expect("seed log");
    }
}

async fn log_count(pool: &exec::DbPool, job_id: &str) -> i64 {
    exec::fetch_optional(
        pool,
        "SELECT COUNT(*) FROM job_logs WHERE job_id = ?",
        &[job_id.into()],
    )
    .await
    .expect("count logs")
    .expect("row")
    .i64(0)
}

async fn purged_flag(pool: &exec::DbPool, job_id: &str) -> i64 {
    exec::fetch_optional(
        pool,
        "SELECT logs_purged FROM jobs WHERE id = ?",
        &[job_id.into()],
    )
    .await
    .expect("load flag")
    .expect("row")
    .i64(0)
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

    let purged = purge_expired_job_logs(&pool.clone(), &broadcaster)
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
        purge_expired_job_logs(&pool.clone(), &broadcaster)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        purge_expired_job_logs(&pool.clone(), &broadcaster)
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
        purge_expired_job_logs(&pool.clone(), &broadcaster)
            .await
            .unwrap(),
        0
    );
    assert_eq!(log_count(&pool, "old").await, 2);

    // Override to 1 day: the 40-day-old job now expires.
    set_retention_days(&pool, "1").await;
    assert_eq!(
        purge_expired_job_logs(&pool.clone(), &broadcaster)
            .await
            .unwrap(),
        1
    );
    assert_eq!(log_count(&pool, "old").await, 0);
}
