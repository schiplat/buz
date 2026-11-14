//! The runner log-upload gate: chunks are accepted only from the runner the
//! job was dispatched to.
//!
//! Regression: `jobs.runner_id` is nullable (a pending job has no runner), and
//! the read used to decode the column as a non-null string. An un-dispatched
//! job must be refused with 403 — never a decode panic and never a write.

mod common;

use axum::http::StatusCode;
use common::{TestApp, RUNNER};
use manager::store::exec;

/// Body of one log chunk (`shared::protocol::LogChunk`).
fn chunk(step_order: i32, chunk_index: i32) -> serde_json::Value {
    serde_json::json!({
        "chunk_index": chunk_index,
        "step_order": step_order,
        "content": "hello from the runner\n",
        "level": "info",
    })
}

async fn insert_job(app: &TestApp, pipeline_id: &str, runner_id: Option<&str>) {
    exec::execute(
        &app.pool,
        "INSERT INTO pipelines (id, name) VALUES (?, 'log upload guard')",
        &[pipeline_id.into()],
    )
    .await
    .expect("insert pipeline");
    exec::execute(
        &app.pool,
        "INSERT INTO jobs (id, pipeline_id, runner_id, status, trigger_by, snapshot, created_at, updated_at)
         VALUES (?, ?, ?, 'pending', 'manual', '{}', datetime('now'), datetime('now'))",
        &[
            format!("job-{pipeline_id}").into(),
            pipeline_id.into(),
            runner_id.into(),
        ],
    )
    .await
    .expect("insert job");
}

#[tokio::test]
async fn undispatched_job_rejects_log_upload() {
    let app = TestApp::boot().await;
    insert_job(&app, "pipe-no-runner", None).await;
    let runner = app.jwt(RUNNER, shared::auth::Role::Runner);

    let (status, _) = app
        .post(
            "/api/v1/jobs/job-pipe-no-runner/logs",
            Some(&runner),
            chunk(0, 0),
        )
        .await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a job with no assigned runner must refuse log uploads"
    );
}

#[tokio::test]
async fn other_runners_job_rejects_log_upload() {
    let app = TestApp::boot().await;
    exec::execute(
        &app.pool,
        "INSERT INTO runners (id, name, register_token, labels, status) \
         VALUES ('runner-not-this-one', 'r2', 'tok2', '[]', 'online')",
        &[],
    )
    .await
    .expect("insert second runner");
    insert_job(&app, "pipe-other-runner", Some("runner-not-this-one")).await;
    let runner = app.jwt(RUNNER, shared::auth::Role::Runner);

    let (status, _) = app
        .post(
            "/api/v1/jobs/job-pipe-other-runner/logs",
            Some(&runner),
            chunk(0, 0),
        )
        .await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a job dispatched to another runner must refuse log uploads"
    );
}

#[tokio::test]
async fn assigned_runner_may_upload() {
    let app = TestApp::boot().await;
    insert_job(&app, "pipe-own", Some(RUNNER)).await;
    let runner = app.jwt(RUNNER, shared::auth::Role::Runner);

    let (status, _) = app
        .post("/api/v1/jobs/job-pipe-own/logs", Some(&runner), chunk(0, 0))
        .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "the assigned runner must be allowed to upload"
    );
}
