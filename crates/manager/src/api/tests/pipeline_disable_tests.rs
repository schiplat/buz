//! Idempotent pipeline disable/enable: disable blocks every trigger path with
//! 409, optionally cancels all in-flight jobs (pending / queued / running /
//! waiting_approval) in the same transaction, and never re-enables on repeat.
//! Enable is a no-op when active. Both require PipelineWrite (Developer+).
//! Included via `api/mod.rs` under `#[cfg(test)]`.

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use sqlx::Row;
use tower::ServiceExt;

use crate::api;
use crate::config::{AuthConfig, Config, PipelineConfig, RunnerConfig, ServerConfig};
use crate::db::run_migrations;
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const SUPER_ADMIN_ID: &str = "admin-1";
const DEV_ID: &str = "dev-1";
const VIEWER_ID: &str = "viewer-1";

fn test_config() -> Config {
    Config {
        server: ServerConfig::default(),
        database: Default::default(),
        auth: AuthConfig {
            jwt_secret: JWT_SECRET.into(),
            jwt_ttl_hours: 24,
            runner_register_token_ttl_hours: 24,
        },
        runner: RunnerConfig::default(),
        pipeline: PipelineConfig::default(),
        standalone: Default::default(),
        step_index: Default::default(),
        storage: Default::default(),
        clock: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
    }
}

async fn test_app() -> (Router, SqlitePool, AppState) {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

    for (id, username) in [
        (SUPER_ADMIN_ID, "boss"),
        (DEV_ID, "dev"),
        (VIEWER_ID, "viewer"),
    ] {
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
             VALUES (?, ?, 'hash', ?, 0, 0, 1)",
        )
        .bind(id)
        .bind(username)
        .bind(username)
        .execute(&pool)
        .await
        .expect("seed user");
    }
    // boss becomes platform admin via flag flip (kept separate for clarity).
    sqlx::query("UPDATE users SET is_platform_admin = 1 WHERE id = ?")
        .bind(SUPER_ADMIN_ID)
        .execute(&pool)
        .await
        .expect("promote admin");

    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')")
        .execute(&pool)
        .await
        .expect("seed team");

    sqlx::query(
        "INSERT INTO team_members (team_id, user_id, role) VALUES ('team-1', ?, 'developer')",
    )
    .bind(DEV_ID)
    .execute(&pool)
    .await
    .expect("seed developer membership");
    sqlx::query("INSERT INTO team_members (team_id, user_id, role) VALUES ('team-1', ?, 'viewer')")
        .bind(VIEWER_ID)
        .execute(&pool)
        .await
        .expect("seed viewer membership");

    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, max_concurrent_jobs)
         VALUES ('runner-1', 'r1', 'tok', '[\"linux-x64\"]', 'online', 4)",
    )
    .execute(&pool)
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

async fn job_status(pool: &SqlitePool, job_id: &str) -> String {
    sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("load job status")
}

/// Move an API-triggered job into an arbitrary non-terminal state, optionally
/// stamping the waiting_approval snapshot marker.
async fn force_status(pool: &SqlitePool, job_id: &str, status: &str, step_statuses: &str) {
    let mut snapshot: serde_json::Value =
        sqlx::query_scalar("SELECT snapshot FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(pool)
            .await
            .expect("load snapshot");
    if status == "waiting_approval" {
        snapshot["approval"] = serde_json::json!({ "required": true, "received": 1 });
    }
    sqlx::query("UPDATE jobs SET status = ?, step_statuses = ? WHERE id = ?")
        .bind(status)
        .bind(step_statuses)
        .bind(job_id)
        .execute(pool)
        .await
        .expect("force status");
    if status == "waiting_approval" {
        sqlx::query("UPDATE jobs SET snapshot = ? WHERE id = ?")
            .bind(serde_json::to_string(&snapshot).unwrap())
            .bind(job_id)
            .execute(pool)
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
    let ss: String = sqlx::query_scalar("SELECT step_statuses FROM jobs WHERE id = ?")
        .bind(&running)
        .fetch_one(&pool)
        .await
        .expect("load step_statuses");
    let ss: serde_json::Value = serde_json::from_str(&ss).expect("parse step_statuses");
    assert_eq!(
        ss[0]["status"], "cancelled",
        "running step marked cancelled"
    );

    // waiting_approval job: snapshot outcome + job_approvals cancel row.
    assert_eq!(job_status(&pool, &waiting).await, "cancelled");
    let snapshot: String = sqlx::query_scalar("SELECT snapshot FROM jobs WHERE id = ?")
        .bind(&waiting)
        .fetch_one(&pool)
        .await
        .expect("load snapshot");
    let snapshot: serde_json::Value = serde_json::from_str(&snapshot).expect("parse snapshot");
    assert_eq!(snapshot["approval"]["outcome"], "cancelled");
    let action: String = sqlx::query_scalar("SELECT action FROM job_approvals WHERE job_id = ?")
        .bind(&waiting)
        .fetch_one(&pool)
        .await
        .expect("approval row exists");
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
    let audit_row = sqlx::query(
        "SELECT action, meta_json FROM audit_logs WHERE action = 'pipeline.disable' AND resource_id = ?",
    )
    .bind(&pipe_a)
    .fetch_one(&pool)
    .await
    .expect("audit row exists");
    let audit_action: String = audit_row.get(0);
    let meta: String = audit_row.get(1);
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
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-2', 'Other', 'other')")
        .execute(&pool)
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
    let team_after_block: String = sqlx::query_scalar("SELECT team_id FROM pipelines WHERE id = ?")
        .bind(&pipe)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(team_after_block, "team-1");

    // Freeze the job's team into the snapshot, then finish it (terminal jobs
    // do not block migration).
    sqlx::query("UPDATE jobs SET status = 'success' WHERE id = ?")
        .bind(&job_id)
        .execute(&pool)
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
    let team_after_move: String = sqlx::query_scalar("SELECT team_id FROM pipelines WHERE id = ?")
        .bind(&pipe)
        .fetch_one(&pool)
        .await
        .unwrap();
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
    let frozen_team: Option<String> =
        sqlx::query_scalar("SELECT json_extract(snapshot, '$.team_id') FROM jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        frozen_team.as_deref(),
        Some("team-1"),
        "snapshot records trigger-time team"
    );
}
