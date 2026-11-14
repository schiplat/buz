//! `GET /jobs/{id}/logs` windowing: `limit`/`offset` bound the response;
//! without them the legacy full read applies (the dashboard relies on it).

mod common;

use axum::http::StatusCode;
use common::{data, TestApp};
use manager::store::exec;

async fn job_with_logs(count: i64) -> (TestApp, String) {
    let app = TestApp::boot().await;
    exec::execute(
        &app.pool,
        "INSERT INTO pipelines (id, name, team_id) VALUES ('p-logs', 'demo', 'team-1')",
        &[],
    )
    .await
    .expect("seed pipeline");
    exec::execute(
        &app.pool,
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot) VALUES ('j-logs', 'p-logs', 'running', 'test', '{}')",
        &[],
    )
    .await
    .expect("seed job");
    for chunk in 0..count {
        exec::execute(
            &app.pool,
            "INSERT INTO job_logs (id, job_id, chunk_index, step_order, log_content) VALUES (?, 'j-logs', ?, 0, ?)",
            &[format!("log-{chunk}").into(), chunk.into(), format!("line-{chunk}").into()],
        )
        .await
        .expect("seed log");
    }
    (app, "j-logs".to_string())
}

async fn get_logs(app: &TestApp, job: &str, query: &str) -> Vec<serde_json::Value> {
    let suffix = if query.is_empty() {
        String::new()
    } else {
        format!("?{query}")
    };
    let admin = app.admin_jwt();
    let (status, resp) = app
        .get(&format!("/api/v1/jobs/{job}/logs{suffix}"), Some(&admin))
        .await;
    assert_eq!(status, StatusCode::OK, "get logs: {resp}");
    let arr = data(&resp);
    arr.as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn full_read_remains_the_default() {
    let (app, job) = job_with_logs(5).await;
    let logs = get_logs(&app, &job, "").await;
    assert_eq!(logs.len(), 5, "no params = legacy full read");
    assert_eq!(logs[0]["content"].as_str().unwrap(), "line-0");
    assert_eq!(logs[4]["content"].as_str().unwrap(), "line-4");
}

#[tokio::test]
async fn limit_and_offset_window_the_rows() {
    let (app, job) = job_with_logs(5).await;

    let logs = get_logs(&app, &job, "limit=2").await;
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0]["content"].as_str().unwrap(), "line-0");
    assert_eq!(logs[1]["content"].as_str().unwrap(), "line-1");

    let logs = get_logs(&app, &job, "limit=2&offset=2").await;
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0]["content"].as_str().unwrap(), "line-2");
    assert_eq!(logs[1]["content"].as_str().unwrap(), "line-3");

    // Offset past the end and oversized limits clamp, never error.
    let logs = get_logs(&app, &job, "limit=2&offset=50").await;
    assert!(logs.is_empty());
    let logs = get_logs(&app, &job, "limit=99999").await;
    assert_eq!(logs.len(), 5, "limit clamps to the row count");
}

#[tokio::test]
async fn tail_returns_the_last_rows_in_ascending_order() {
    let (app, job) = job_with_logs(5).await;

    let logs = get_logs(&app, &job, "tail=2").await;
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0]["content"].as_str().unwrap(), "line-3");
    assert_eq!(logs[1]["content"].as_str().unwrap(), "line-4");

    // Oversized tail clamps to the row count, full history ascending.
    let logs = get_logs(&app, &job, "tail=999").await;
    assert_eq!(logs.len(), 5);
    assert_eq!(logs[0]["content"].as_str().unwrap(), "line-0");
}

#[tokio::test]
async fn before_cursor_back_pages_through_the_log() {
    let (app, job) = job_with_logs(5).await;

    // Screen 1: tail=2 → (0,3), (0,4). Back page: strictly before (0,3).
    let logs = get_logs(&app, &job, "tail=2&before_step=0&before_chunk=3").await;
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0]["content"].as_str().unwrap(), "line-1");
    assert_eq!(logs[1]["content"].as_str().unwrap(), "line-2");

    // Back page past the start clamps to whatever exists.
    let logs = get_logs(&app, &job, "tail=2&before_step=0&before_chunk=1").await;
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["content"].as_str().unwrap(), "line-0");
}

#[tokio::test]
async fn half_cursor_is_rejected() {
    let (app, job) = job_with_logs(2).await;
    let admin = app.admin_jwt();
    let (status, resp) = app
        .get(
            &format!("/api/v1/jobs/{job}/logs?before_chunk=3"),
            Some(&admin),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "half cursor: {resp}"
    );
}
