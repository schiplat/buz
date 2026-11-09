//! `GET /jobs/{id}/logs` windowing: `limit`/`offset` bound the response;
//! without them the legacy full read applies (the dashboard relies on it).

mod common;

use axum::http::StatusCode;
use common::{data, TestApp};

async fn job_with_logs(count: i64) -> (TestApp, String) {
    let app = TestApp::boot().await;
    sqlx::query("INSERT INTO pipelines (id, name, team_id) VALUES ('p-logs', 'demo', 'team-1')")
        .execute(&app.pool)
        .await
        .expect("seed pipeline");
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot) VALUES ('j-logs', 'p-logs', 'running', 'test', '{}')",
    )
    .execute(&app.pool)
    .await
    .expect("seed job");
    for chunk in 0..count {
        sqlx::query(
            "INSERT INTO job_logs (id, job_id, chunk_index, step_order, log_content) VALUES (?, 'j-logs', ?, 0, ?)",
        )
        .bind(format!("log-{chunk}"))
        .bind(chunk)
        .bind(format!("line-{chunk}"))
        .execute(&app.pool)
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
