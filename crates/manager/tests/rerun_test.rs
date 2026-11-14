//! `POST /jobs/{id}/rerun` — snapshot replay semantics: same run_number,
//! attempt + 1, inputs carried over from the frozen snapshot, terminal-only
//! source, debug records admin-only, protected environments re-gate.

mod common;

use axum::http::StatusCode;
use common::{data, TestApp};
use manager::store::exec;

async fn create_pipeline(app: &TestApp, jwt: &str, environments: &[&str]) -> String {
    let (status, body) = app
        .post(
            "/api/v1/pipelines",
            Some(jwt),
            serde_json::json!({
                "name": "rerun-demo",
                "trigger_type": "manual",
                "runner_labels": ["linux-x64"],
                "team_id": "team-1",
                "environments": environments,
                "env": [],
                "input_defs": [
                    { "name": "tag", "required": false, "type": "string" }
                ],
                "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "create pipeline: {body}");
    data(&body)["id"].as_str().unwrap().to_string()
}

async fn trigger(app: &TestApp, jwt: &str, pipeline: &str, body: serde_json::Value) -> String {
    let (status, resp) = app
        .post(
            &format!("/api/v1/pipelines/{pipeline}/trigger"),
            Some(jwt),
            body,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "trigger: {resp}");
    data(&resp)["job_id"].as_str().unwrap().to_string()
}

async fn force_terminal(app: &TestApp, job: &str, status: &str) {
    // Status values are the test's own literals (never caller text), so the
    // interpolation stays literal-safe; the job id stays bound.
    exec::execute(
        &app.pool,
        &format!("UPDATE jobs SET status = '{status}', end_time = datetime('now') WHERE id = ?"),
        &[job.into()],
    )
    .await
    .expect("force terminal status");
}

async fn job_row(app: &TestApp, job: &str) -> serde_json::Value {
    let (status, resp) = app
        .get(&format!("/api/v1/jobs/{job}"), Some(&app.admin_jwt()))
        .await;
    assert_eq!(status, StatusCode::OK, "get job: {resp}");
    data(&resp)
}

/// Happy path: same run_number, attempt + 1, snapshot replay provenance,
/// and sealed inputs carried over (non-masking values visible via get_one).
#[tokio::test]
async fn rerun_reuses_run_number_and_carries_inputs() {
    let app = TestApp::boot().await;
    let admin = app.admin_jwt();
    let pid = create_pipeline(&app, &admin, &["default"]).await;
    let source = trigger(
        &app,
        &admin,
        &pid,
        serde_json::json!({ "inputs": { "tag": "v9" }, "environment": "default" }),
    )
    .await;
    force_terminal(&app, &source, "failed").await;

    let source_row = job_row(&app, &source).await;
    let run_number = source_row["run_number"].as_i64().unwrap();

    let (status, resp) = app
        .post(
            &format!("/api/v1/jobs/{source}/rerun"),
            Some(&admin),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "rerun: {resp}");
    let rerun = data(&resp);
    assert_eq!(rerun["rerun_of"].as_str().unwrap(), source);
    assert_eq!(
        rerun["run_number"].as_i64().unwrap(),
        run_number,
        "same run_number"
    );
    assert_eq!(
        rerun["run_attempt"].as_i64().unwrap(),
        2,
        "attempt increments"
    );
    assert_eq!(rerun["status"].as_str().unwrap(), "pending");

    let new_job = rerun["job_id"].as_str().unwrap();
    let row = job_row(&app, new_job).await;
    assert_eq!(row["run_number"].as_i64().unwrap(), run_number);
    assert_eq!(row["run_attempt"].as_i64().unwrap(), 2);
    assert_eq!(
        row["snapshot"]["rerun"]["of"].as_str().unwrap(),
        source,
        "snapshot records the replay provenance"
    );
    assert_eq!(
        row["snapshot"]["triggered_by"].as_str().unwrap(),
        "boss",
        "trigger identity is the re-run actor"
    );
    // get_one returns trigger_params as a JSON-encoded string (redacted view).
    let params: serde_json::Value =
        serde_json::from_str(row["trigger_params"].as_str().unwrap_or("null")).unwrap_or_default();
    assert_eq!(
        params["tag"].as_str(),
        Some("v9"),
        "inputs carried over from the frozen snapshot"
    );
}

/// Non-terminal sources are refused.
#[tokio::test]
async fn rerun_rejects_non_terminal_source() {
    let app = TestApp::boot().await;
    let admin = app.admin_jwt();
    let pid = create_pipeline(&app, &admin, &["default"]).await;
    let job = trigger(
        &app,
        &admin,
        &pid,
        serde_json::json!({ "environment": "default" }),
    )
    .await;

    let (status, resp) = app
        .post(
            &format!("/api/v1/jobs/{job}/rerun"),
            Some(&admin),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "pending source: {resp}");

    force_terminal(&app, &job, "failed").await;
    let (status, _) = app
        .post(
            &format!("/api/v1/jobs/{job}/rerun"),
            Some(&admin),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "terminal source reruns");
}

/// Debug (unredacted) jobs stay admin-invisible: a non-admin gets 404, the
/// admin can re-run.
#[tokio::test]
async fn rerun_of_debug_job_is_admin_only() {
    let app = TestApp::boot().await;
    let admin = app.admin_jwt();
    let member = app.member_jwt();
    let pid = create_pipeline(&app, &admin, &["default"]).await;
    let job = trigger(
        &app,
        &admin,
        &pid,
        serde_json::json!({ "debug": true, "environment": "default" }),
    )
    .await;
    force_terminal(&app, &job, "success").await;

    let (status, resp) = app
        .post(
            &format!("/api/v1/jobs/{job}/rerun"),
            Some(&member),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "non-admin cannot see debug jobs: {resp}"
    );

    let (status, resp) = app
        .post(
            &format!("/api/v1/jobs/{job}/rerun"),
            Some(&admin),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "admin re-runs the debug job: {resp}"
    );
}

/// Current environment protection gates the replay: a job of a protected
/// environment re-enters waiting_approval even though the source was
/// approved long ago.
#[tokio::test]
async fn rerun_enters_waiting_approval_for_protected_environment() {
    let app = TestApp::boot().await;
    let admin = app.admin_jwt();
    let (status, resp) = app
        .post(
            "/api/v1/environments",
            Some(&admin),
            serde_json::json!({
                "name": "prod-rerun",
                "slug": "prod-rerun",
                "protection_enabled": true,
                "min_trigger_role": "developer",
                "min_approve_role": "maintainer",
                "require_approvals": 1,
                "forbid_self_approve": true,
                "approval_timeout_seconds": 86400
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "create protected env: {resp}");

    let pid = create_pipeline(&app, &admin, &["prod-rerun"]).await;
    let job = trigger(
        &app,
        &admin,
        &pid,
        serde_json::json!({ "environment": "prod-rerun" }),
    )
    .await;
    force_terminal(&app, &job, "cancelled").await;

    let (status, resp) = app
        .post(
            &format!("/api/v1/jobs/{job}/rerun"),
            Some(&admin),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "rerun: {resp}");
    let rerun = data(&resp);
    assert_eq!(rerun["status"].as_str().unwrap(), "waiting_approval");
    assert_eq!(rerun["approval_required"].as_bool(), Some(true));

    let row = job_row(&app, rerun["job_id"].as_str().unwrap()).await;
    assert_eq!(
        row["snapshot"]["approval"]["received"].as_i64(),
        Some(0),
        "approval ledger resets on the replay"
    );
    assert_eq!(
        row["snapshot"]["protection"]["enabled"].as_bool(),
        Some(true),
        "current environment protection is frozen into the replay"
    );
}

/// A disabled pipeline refuses re-runs.
#[tokio::test]
async fn rerun_refuses_disabled_pipeline() {
    let app = TestApp::boot().await;
    let admin = app.admin_jwt();
    let pid = create_pipeline(&app, &admin, &["default"]).await;
    let job = trigger(
        &app,
        &admin,
        &pid,
        serde_json::json!({ "environment": "default" }),
    )
    .await;
    force_terminal(&app, &job, "failed").await;

    let (status, _) = app
        .post(
            &format!("/api/v1/pipelines/{pid}/disable"),
            Some(&admin),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "disable pipeline");

    let (status, resp) = app
        .post(
            &format!("/api/v1/jobs/{job}/rerun"),
            Some(&admin),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "disabled pipeline: {resp}");
}

/// Team-scoped: a member with PipelineTrigger on team-1 can re-run;
/// audit coverage for job.rerun is enforced by audit_coverage_test.
#[tokio::test]
async fn member_can_rerun_team_pipeline() {
    let app = TestApp::boot().await;
    let admin = app.admin_jwt();
    let member = app.member_jwt();
    let pid = create_pipeline(&app, &admin, &["default"]).await;
    let job = trigger(
        &app,
        &admin,
        &pid,
        serde_json::json!({ "environment": "default" }),
    )
    .await;
    force_terminal(&app, &job, "failed").await;

    let (status, resp) = app
        .post(
            &format!("/api/v1/jobs/{job}/rerun"),
            Some(&member),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "team member re-runs: {resp}");
}
