//! Idempotent pipeline disable/enable: disable blocks every trigger path with
//! 409, optionally cancels all in-flight jobs (pending / queued / running /
//! waiting_approval) in the same transaction, and never re-enables on repeat.
//! Enable is a no-op when active. Both require PipelineWrite (Developer+).

mod common;

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use manager::store::exec;
use shared::auth::{Role, TokenManager};
use tower::ServiceExt;

use manager::api;
use manager::config::AuthConfig;
use manager::config::Config;
use manager::config::PipelineConfig;
use manager::config::RunnerConfig;
use manager::config::ServerConfig;
use manager::services::log_broadcast::LogBroadcaster;
use manager::state::AppState;

/// Form retirement: create/update write a YAML file under `yaml_scan_dir`.
/// Tests in one binary run in parallel, so each gets its own directory (leaked
/// on purpose — the process exits after the test binary finishes).
fn isolated_pipeline_config() -> PipelineConfig {
    let path = tempfile::tempdir().expect("temp scan dir").keep();
    PipelineConfig {
        yaml_scan_dir: path.to_string_lossy().into_owned(),
        ..PipelineConfig::default()
    }
}

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const SUPER_ADMIN_ID: &str = "admin-1";
const DEV_ID: &str = "dev-1";
const VIEWER_ID: &str = "viewer-1";

fn test_config() -> Config {
    Config {
        server: ServerConfig::default(),
        database: Default::default(),
        cluster: Default::default(),
        auth: AuthConfig {
            jwt_secret: JWT_SECRET.into(),
            jwt_ttl_hours: 24,
            runner_register_token_ttl_hours: 24,
        },
        runner: RunnerConfig::default(),
        pipeline: isolated_pipeline_config(),
        standalone: Default::default(),
        plugin_index: Default::default(),
        storage: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
    }
}

async fn test_app() -> (Router, exec::DbPool, AppState) {
    let pool = common::test_db(4).await;

    for (id, username) in [
        (SUPER_ADMIN_ID, "boss"),
        (DEV_ID, "dev"),
        (VIEWER_ID, "viewer"),
    ] {
        exec::execute(&pool,
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
             VALUES (?, ?, 'hash', ?, 0, 0, 1)",
        &[id.into(), username.into(), username.into()],
    )
        .await
        .expect("seed user");
    }
    // boss becomes platform admin via flag flip (kept separate for clarity).
    exec::execute(
        &pool,
        "UPDATE users SET is_platform_admin = 1 WHERE id = ?",
        &[SUPER_ADMIN_ID.into()],
    )
    .await
    .expect("promote admin");

    exec::execute(
        &pool,
        "INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')",
        &[],
    )
    .await
    .expect("seed team");

    exec::execute(
        &pool,
        "INSERT INTO team_members (team_id, user_id, role) VALUES ('team-1', ?, 'developer')",
        &[DEV_ID.into()],
    )
    .await
    .expect("seed developer membership");
    exec::execute(
        &pool,
        "INSERT INTO team_members (team_id, user_id, role) VALUES ('team-1', ?, 'viewer')",
        &[VIEWER_ID.into()],
    )
    .await
    .expect("seed viewer membership");

    exec::execute(
        &pool,
        "INSERT INTO runners (id, name, register_token, labels, status, max_concurrent_jobs)
         VALUES ('runner-1', 'r1', 'tok', '[\"linux-x64\"]', 'online', 4)",
        &[],
    )
    .await
    .expect("seed runner");

    let state = AppState {
        pool: pool.clone(),
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    let app = Router::new().nest("/api/v1", api::router(state.clone()));
    (app, pool, state)
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn admin_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(SUPER_ADMIN_ID, Role::Admin, 1)
        .expect("mint admin jwt")
}

fn dev_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(DEV_ID, Role::User, 0)
        .expect("mint developer jwt")
}

fn viewer_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(VIEWER_ID, Role::User, 0)
        .expect("mint viewer jwt")
}

async fn read_body(response: axum::response::Response) -> serde_json::Value {
    let body = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    serde_json::from_slice(&body).expect("json body")
}

/// Send a request with an optional JSON body. `None` sends no body and no
/// Content-Type header — the `Option<Json<_>>` extractor then yields `None`.
async fn json_req(
    app: &Router,
    method: &str,
    uri: &str,
    jwt: &str,
    body: Option<serde_json::Value>,
) -> axum::response::Response {
    let mut builder = Request::builder().uri(uri).method(method);
    if !jwt.is_empty() {
        builder = builder.header(AUTHORIZATION, bearer(jwt));
    }
    match body {
        Some(v) => {
            let bytes = serde_json::to_vec(&v).expect("serialize body");
            builder = builder.header("content-type", "application/json");
            app.clone()
                .oneshot(builder.body(Body::from(bytes)).unwrap())
                .await
                .unwrap()
        }
        None => app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap(),
    }
}

async fn create_env(app: &Router, jwt: &str, slug: &str) {
    let create = json_req(
        app,
        "POST",
        "/api/v1/environments",
        jwt,
        Some(serde_json::json!({
            "name": slug,
            "slug": slug,
            "protection_enabled": false,
            "deployment_concurrency": "off",
        })),
    )
    .await;
    assert_eq!(create.status(), StatusCode::OK, "create env {slug}");
}

async fn create_pipeline(
    app: &Router,
    jwt: &str,
    name: &str,
    environment: &str,
    labels: &[&str],
) -> String {
    let pipe = json_req(
        app,
        "POST",
        "/api/v1/pipelines",
        jwt,
        Some(serde_json::json!({
            "name": name,
            "trigger_type": "manual",
            "runner_labels": labels,
            "team_id": "team-1",
            "environments": [environment],
            "env": [],
            "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
        })),
    )
    .await;
    assert_eq!(pipe.status(), StatusCode::OK, "create pipeline {name}");
    read_body(pipe).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn trigger_job(app: &Router, jwt: &str, pipeline_id: &str, environment: &str) -> String {
    let trig = json_req(
        app,
        "POST",
        &format!("/api/v1/pipelines/{pipeline_id}/trigger"),
        jwt,
        Some(serde_json::json!({ "inputs": {}, "environment": environment })),
    )
    .await;
    assert_eq!(trig.status(), StatusCode::OK, "trigger pipeline");
    read_body(trig).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn job_status(pool: &exec::DbPool, job_id: &str) -> String {
    exec::fetch_optional(
        pool,
        "SELECT status FROM jobs WHERE id = ?",
        &[job_id.into()],
    )
    .await
    .expect("load job status")
    .expect("row")
    .text(0)
}

/// Move an API-triggered job into an arbitrary non-terminal state, optionally
/// stamping the waiting_approval snapshot marker.
async fn force_status(pool: &exec::DbPool, job_id: &str, status: &str, step_statuses: &str) {
    let mut snapshot: serde_json::Value = serde_json::from_str(
        &exec::fetch_optional(
            pool,
            "SELECT snapshot FROM jobs WHERE id = ?",
            &[job_id.into()],
        )
        .await
        .unwrap()
        .expect("load snapshot")
        .text(0),
    )
    .unwrap();
    if status == "waiting_approval" {
        snapshot["approval"] = serde_json::json!({ "required": true, "received": 1 });
    }
    exec::execute(
        pool,
        "UPDATE jobs SET status = ?, step_statuses = ? WHERE id = ?",
        &[status.into(), step_statuses.into(), job_id.into()],
    )
    .await
    .expect("force status");
    if status == "waiting_approval" {
        exec::execute(
            pool,
            "UPDATE jobs SET snapshot = ? WHERE id = ?",
            &[
                serde_json::to_string(&snapshot).unwrap().into(),
                job_id.into(),
            ],
        )
        .await
        .expect("stamp approval metadata");
    }
}

async fn disable(
    app: &Router,
    jwt: &str,
    pipeline_id: &str,
    cancel_in_flight: Option<bool>,
) -> axum::response::Response {
    let body = cancel_in_flight.map(|v| serde_json::json!({ "cancel_in_flight": v }));
    json_req(
        app,
        "POST",
        &format!("/api/v1/pipelines/{pipeline_id}/disable"),
        jwt,
        body,
    )
    .await
}

#[tokio::test]
async fn disable_cancels_in_flight_jobs_and_blocks_trigger() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();

    create_env(&app, &admin, "prod").await;
    let pipe_a = create_pipeline(&app, &admin, "release-a", "prod", &["linux-x64"]).await;
    let pipe_b = create_pipeline(&app, &admin, "release-b", "prod", &["gpu-x64"]).await;

    // One job per non-terminal state on pipe_a, plus a sibling-pipeline job
    // that must survive the disable untouched.
    let running = trigger_job(&app, &admin, &pipe_a, "prod").await;
    force_status(
        &pool,
        &running,
        "running",
        r#"[{"step_order":0,"status":"running","started_at":"2030-01-01 00:00:00"}]"#,
    )
    .await;
    let waiting = trigger_job(&app, &admin, &pipe_a, "prod").await;
    force_status(&pool, &waiting, "waiting_approval", "[]").await;
    let untouched = trigger_job(&app, &admin, &pipe_b, "prod").await;

    let resp = disable(&app, &admin, &pipe_a, Some(true)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["data"]["is_active"], false);
    assert_eq!(body["data"]["cancelled_count"], 2);
    let ids = body["data"]["cancelled_jobs"].as_array().expect("job ids");
    assert!(
        ids.contains(&serde_json::json!(running)),
        "running job listed"
    );
    assert!(
        ids.contains(&serde_json::json!(waiting)),
        "waiting_approval job listed"
    );

    // Running job: overall status + running step flipped to cancelled.
    assert_eq!(job_status(&pool, &running).await, "cancelled");
    let ss: String = exec::fetch_optional(
        &pool,
        "SELECT step_statuses FROM jobs WHERE id = ?",
        &[running.as_str().into()],
    )
    .await
    .unwrap()
    .expect("load step_statuses")
    .text(0);
    let ss: serde_json::Value = serde_json::from_str(&ss).expect("parse step_statuses");
    assert_eq!(
        ss[0]["status"], "cancelled",
        "running step marked cancelled"
    );

    // waiting_approval job: snapshot outcome + job_approvals cancel row.
    assert_eq!(job_status(&pool, &waiting).await, "cancelled");
    let snapshot: String = exec::fetch_optional(
        &pool,
        "SELECT snapshot FROM jobs WHERE id = ?",
        &[waiting.as_str().into()],
    )
    .await
    .unwrap()
    .expect("load snapshot")
    .text(0);
    let snapshot: serde_json::Value = serde_json::from_str(&snapshot).expect("parse snapshot");
    assert_eq!(snapshot["approval"]["outcome"], "cancelled");
    let action: String = exec::fetch_optional(
        &pool,
        "SELECT action FROM job_approvals WHERE job_id = ?",
        &[waiting.as_str().into()],
    )
    .await
    .unwrap()
    .expect("approval row exists")
    .text(0);
    assert_eq!(action, "cancel");

    // Sibling pipeline's job is untouched.
    assert_eq!(job_status(&pool, &untouched).await, "pending");

    // Disabled pipeline rejects manual trigger with 409, sibling still works.
    let blocked = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipe_a}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "prod" })),
    )
    .await;
    assert_eq!(blocked.status(), StatusCode::CONFLICT);
    let ok = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipe_b}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "prod" })),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);

    // Audit trail: one disable event naming the cancelled jobs.
    let audit_row = exec::fetch_optional(
        &pool,
        "SELECT action, meta_json FROM audit_logs WHERE action = 'pipeline.disable' AND resource_id = ?",
        &[pipe_a.as_str().into()],
    )
    .await
    .unwrap()
    .expect("audit row exists");
    let audit_action: String = audit_row.text(0);
    let meta: String = audit_row.text(1);
    assert_eq!(audit_action, "pipeline.disable");
    assert!(meta.contains(&running), "audit meta lists cancelled job");
}

#[tokio::test]
async fn disable_without_cancel_leaves_in_flight_jobs_running() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();

    create_env(&app, &admin, "prod").await;
    let pipe = create_pipeline(&app, &admin, "release", "prod", &["linux-x64"]).await;
    let job = trigger_job(&app, &admin, &pipe, "prod").await;

    // No body at all: cancel_in_flight defaults to false.
    let resp = disable(&app, &admin, &pipe, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["data"]["is_active"], false);
    assert_eq!(body["data"]["cancelled_count"], 0);
    assert_eq!(job_status(&pool, &job).await, "pending");

    let blocked = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipe}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "prod" })),
    )
    .await;
    assert_eq!(blocked.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn disable_and_enable_are_idempotent() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();

    create_env(&app, &admin, "prod").await;
    let pipe = create_pipeline(&app, &admin, "release", "prod", &["linux-x64"]).await;
    let job = trigger_job(&app, &admin, &pipe, "prod").await;

    let first = disable(&app, &admin, &pipe, Some(true)).await;
    assert_eq!(first.status(), StatusCode::OK);
    let body = read_body(first).await;
    assert_eq!(body["data"]["cancelled_count"], 1);

    // Repeated disable must NOT re-enable (unlike toggle) and finds nothing
    // left to cancel.
    let second = disable(&app, &admin, &pipe, Some(true)).await;
    assert_eq!(second.status(), StatusCode::OK);
    let body = read_body(second).await;
    assert_eq!(body["data"]["is_active"], false);
    assert_eq!(body["data"]["cancelled_count"], 0);

    // Enable twice: both succeed, pipeline triggers again.
    for _ in 0..2 {
        let resp = json_req(
            &app,
            "POST",
            &format!("/api/v1/pipelines/{pipe}/enable"),
            &admin,
            None,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(read_body(resp).await["data"]["is_active"], true);
    }
    let again = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipe}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "prod" })),
    )
    .await;
    assert_eq!(again.status(), StatusCode::OK);

    // The job cancelled by disable stays cancelled after re-enable.
    assert_eq!(job_status(&pool, &job).await, "cancelled");
}

#[tokio::test]
async fn disable_and_enable_require_pipeline_write() {
    let (app, _pool, _state) = test_app().await;
    let admin = admin_jwt();

    create_env(&app, &admin, "prod").await;
    let pipe = create_pipeline(&app, &admin, "release", "prod", &["linux-x64"]).await;

    // Viewer (below Developer) is rejected on both endpoints.
    let resp = disable(&app, &viewer_jwt(), &pipe, Some(true)).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipe}/enable"),
        &viewer_jwt(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // Developer (exact minimum role for PipelineWrite) succeeds.
    let resp = disable(&app, &dev_jwt(), &pipe, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn disable_unknown_pipeline_is_404() {
    let (app, _pool, _state) = test_app().await;
    let resp = disable(&app, &admin_jwt(), "no-such-id", None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pipeline_team_migration_blocked_while_jobs_active_and_allowed_after_terminal() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    exec::execute(
        &pool,
        "INSERT INTO teams (id, name, slug) VALUES ('team-2', 'Other', 'other')",
        &[],
    )
    .await
    .expect("seed second team");

    let pipe = create_pipeline(&app, &admin, "team-migrate", "default", &["linux-x64"]).await;
    let trigger = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipe}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "default" })),
    )
    .await;
    let job_id = read_body(trigger).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    let update_body = serde_json::json!({
        "name": "team-migrate",
        "trigger_type": "manual",
        "runner_labels": ["linux-x64"],
        "team_id": "team-2",
        "environments": ["default"],
        "env": [],
        "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }],
    });
    let blocked = json_req(
        &app,
        "PUT",
        &format!("/api/v1/pipelines/{pipe}"),
        &admin,
        Some(update_body.clone()),
    )
    .await;
    assert_eq!(
        blocked.status(),
        StatusCode::CONFLICT,
        "active job must block team migration"
    );
    let team_after_block: String = exec::fetch_optional(
        &pool,
        "SELECT team_id FROM pipelines WHERE id = ?",
        &[pipe.as_str().into()],
    )
    .await
    .unwrap()
    .unwrap()
    .text(0);
    assert_eq!(team_after_block, "team-1");

    // Freeze the job's team into the snapshot, then finish it (terminal jobs
    // do not block migration).
    exec::execute(
        &pool,
        "UPDATE jobs SET status = 'success' WHERE id = ?",
        &[job_id.into()],
    )
    .await
    .expect("finish job");

    let allowed = json_req(
        &app,
        "PUT",
        &format!("/api/v1/pipelines/{pipe}"),
        &admin,
        Some(update_body),
    )
    .await;
    assert_eq!(
        allowed.status(),
        StatusCode::OK,
        "terminal jobs allow team migration"
    );
    let team_after_move: String = exec::fetch_optional(
        &pool,
        "SELECT team_id FROM pipelines WHERE id = ?",
        &[pipe.as_str().into()],
    )
    .await
    .unwrap()
    .unwrap()
    .text(0);
    assert_eq!(team_after_move, "team-2");
}

#[tokio::test]
async fn job_snapshot_freezes_trigger_time_team() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let pipe = create_pipeline(&app, &admin, "team-freeze", "default", &["linux-x64"]).await;
    let trigger = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipe}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "default" })),
    )
    .await;
    assert_eq!(trigger.status(), StatusCode::OK);
    let job_id = read_body(trigger).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    // json_extract is SQLite-only: read the snapshot and parse in Rust.
    let frozen_team: Option<String> = exec::fetch_optional(
        &pool,
        "SELECT snapshot FROM jobs WHERE id = ?",
        &[job_id.into()],
    )
    .await
    .unwrap()
    .and_then(|r| {
        let raw = r.text(0);
        serde_json::from_str::<serde_json::Value>(&raw)
            .unwrap_or_default()
            .get("team_id")
            .and_then(|v| v.as_str().map(str::to_string))
    });
    assert_eq!(
        frozen_team.as_deref(),
        Some("team-1"),
        "snapshot records trigger-time team"
    );
}

/// The editor's lint endpoint must reject a `uses:` reference nobody can
/// resolve here — that is the whole point of running the check while the
/// author is still typing.
#[tokio::test]
async fn validate_reports_unknown_plugin_with_the_reference_in_the_message() {
    let (_app, _pool, state) = test_app().await;
    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(SUPER_ADMIN_ID, Role::Admin, 1)
        .unwrap();
    // Empty local plugin directory: nothing is available.
    let empty = tempfile::tempdir().unwrap();
    let mut state = state;
    state.config.plugin_index.local_dir = empty.path().to_string_lossy().into_owned();
    let app = axum::Router::new().nest("/api/v1", api::router(state));

    let body = serde_json::json!({
        "yaml": "name: probe\nenvironments: [default]\nsteps:\n  - name: run\n    uses: alice/ghost-plugin@1.0.0\n",
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines/validate")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let raw = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["valid"], false, "{body}");
    let errors = body["data"]["errors"].as_array().expect("errors array");
    let hit = errors
        .iter()
        .find(|error| error["field"] == "steps.0.uses")
        .unwrap_or_else(|| panic!("expected steps.0.uses error, got {errors:?}"));
    assert!(
        hit["message"]
            .as_str()
            .unwrap_or("")
            .contains("alice/ghost-plugin"),
        "{hit}"
    );

    // …and a plugin that IS in the local directory passes the check.
    let plugin_dir = empty.path().join("alice").join("real-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("1.0.0.yaml"),
        "id: alice/real-plugin\nversion: 1.0.0\npublisher: community\nauthor: alice\n\
         source_repo: https://github.com/alice/steps\nengine: shell\ninputs: {}\nrun: echo hi\n",
    )
    .unwrap();
    let body = serde_json::json!({
        "yaml": "name: probe\nenvironments: [default]\nsteps:\n  - name: run\n    uses: alice/real-plugin@1.0.0\n",
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines/validate")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let raw = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let errors = body["data"]["errors"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !errors
            .iter()
            .any(|e| e["field"].as_str().unwrap_or("").starts_with("steps.")),
        "available plugin must not error: {errors:?}"
    );
}
