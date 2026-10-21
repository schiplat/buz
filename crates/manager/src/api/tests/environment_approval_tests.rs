//! Environment approval gates (方案 B): protected trigger, approve/reject,
//! self-approve forbid, automated trigger rejection, poll gating.
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
use std::collections::HashMap;
use tower::ServiceExt;

use crate::api;
use crate::api::pipelines::do_trigger;
use crate::config::{AuthConfig, Config, PipelineConfig, RunnerConfig, ServerConfig};
use crate::db::run_migrations;
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const SUPER_ADMIN_ID: &str = "admin-1";
const DEV_ID: &str = "dev-1";
const MAINTAINER_ID: &str = "maint-1";

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

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 1, 1, 1)",
    )
    .bind(SUPER_ADMIN_ID)
    .bind("boss")
    .bind("boss")
    .execute(&pool)
    .await
    .expect("seed admin");

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 0, 0, 1)",
    )
    .bind(DEV_ID)
    .bind("dev")
    .bind("dev")
    .execute(&pool)
    .await
    .expect("seed developer");

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 0, 0, 1)",
    )
    .bind(MAINTAINER_ID)
    .bind("maint")
    .bind("maint")
    .execute(&pool)
    .await
    .expect("seed maintainer");

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

    sqlx::query(
        "INSERT INTO team_members (team_id, user_id, role) VALUES ('team-1', ?, 'maintainer')",
    )
    .bind(MAINTAINER_ID)
    .execute(&pool)
    .await
    .expect("seed maintainer membership");

    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, max_concurrent_jobs)
         VALUES ('runner-1', 'r1', 'tok', '[\"linux-x64\"]', 'online', 2)",
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

fn maintainer_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(MAINTAINER_ID, Role::User, 0)
        .expect("mint maintainer jwt")
}

fn runner_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token("runner-1", Role::Runner, 1)
        .expect("mint runner jwt")
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

async fn json_req(
    app: &Router,
    method: &str,
    uri: &str,
    jwt: &str,
    body: Option<serde_json::Value>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(AUTHORIZATION, bearer(jwt));
    let req_body = if let Some(b) = body {
        builder = builder.header("Content-Type", "application/json");
        Body::from(serde_json::to_vec(&b).unwrap())
    } else {
        Body::empty()
    };
    app.clone()
        .oneshot(builder.body(req_body).unwrap())
        .await
        .unwrap()
}

async fn create_protected_env(app: &Router, jwt: &str, slug: &str) -> String {
    let create = json_req(
        app,
        "POST",
        "/api/v1/environments",
        jwt,
        Some(serde_json::json!({
            "name": slug,
            "slug": slug,
            "protection_enabled": true,
            "min_trigger_role": "developer",
            "min_approve_role": "maintainer",
            "require_approvals": 1,
            "forbid_self_approve": true,
            "approval_timeout_seconds": 86400,
        })),
    )
    .await;
    assert_eq!(create.status(), StatusCode::OK, "create protected env");
    let body = read_body(create).await;
    assert_eq!(body["data"]["protection_enabled"], true);
    body["data"]["id"].as_str().unwrap().to_string()
}

async fn create_pipeline(app: &Router, jwt: &str, name: &str, environment: &str) -> String {
    let pipe = json_req(
        app,
        "POST",
        "/api/v1/pipelines",
        jwt,
        Some(serde_json::json!({
            "name": name,
            "trigger_type": "manual",
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "environments": [environment],
            "env": [],
            "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
        })),
    )
    .await;
    assert_eq!(pipe.status(), StatusCode::OK, "create pipeline");
    read_body(pipe).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn developer_cannot_trigger_when_min_trigger_is_maintainer() {
    let (app, _pool, _state) = test_app().await;
    let admin = admin_jwt();
    let env_id = create_protected_env(&app, &admin, "staging").await;

    let upd = json_req(
        &app,
        "PUT",
        &format!("/api/v1/environments/{env_id}"),
        &admin,
        Some(serde_json::json!({ "min_trigger_role": "maintainer" })),
    )
    .await;
    assert_eq!(upd.status(), StatusCode::OK);

    let pid = create_pipeline(&app, &admin, "need-maint-trigger", "staging").await;
    let trig = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pid}/trigger"),
        &dev_jwt(),
        Some(serde_json::json!({ "inputs": {}, "environment": "staging" })),
    )
    .await;
    assert_eq!(trig.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn protected_trigger_waits_approval_and_is_not_pollable() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let _env_id = create_protected_env(&app, &admin, "production").await;
    let pid = create_pipeline(&app, &admin, "gated-pipe", "production").await;

    let trig = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pid}/trigger"),
        &dev_jwt(),
        Some(serde_json::json!({ "inputs": {}, "environment": "production" })),
    )
    .await;
    assert_eq!(trig.status(), StatusCode::OK);
    let body = read_body(trig).await;
    assert_eq!(body["data"]["status"], "waiting_approval");
    assert_eq!(body["data"]["approval_required"], true);
    let job_id = body["data"]["job_id"].as_str().unwrap().to_string();

    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "waiting_approval");

    let poll = json_req(&app, "GET", "/api/v1/runners/poll", &runner_jwt(), None).await;
    assert_eq!(poll.status(), StatusCode::OK);
    let poll_body = read_body(poll).await;
    assert!(
        poll_body["data"]["job_id"].is_null(),
        "waiting_approval must not be claimed: {poll_body}"
    );
}

#[tokio::test]
async fn maintainer_approve_promotes_to_pending_and_pollable() {
    let (app, _pool, _state) = test_app().await;
    let admin = admin_jwt();
    let _env_id = create_protected_env(&app, &admin, "prod").await;
    let pid = create_pipeline(&app, &admin, "approve-pipe", "prod").await;

    let trig = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pid}/trigger"),
        &dev_jwt(),
        Some(serde_json::json!({ "inputs": {}, "environment": "prod" })),
    )
    .await;
    let job_id = read_body(trig).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    let approve = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/approve"),
        &maintainer_jwt(),
        Some(serde_json::json!({ "comment": "LGTM" })),
    )
    .await;
    assert_eq!(approve.status(), StatusCode::OK, "approve");
    let approved = read_body(approve).await;
    assert_eq!(approved["data"]["status"], "pending");

    let got = json_req(&app, "GET", &format!("/api/v1/jobs/{job_id}"), &admin, None).await;
    assert_eq!(got.status(), StatusCode::OK);
    let detail = read_body(got).await;
    let approvals = detail["data"]["approvals"].as_array().expect("approvals");
    assert_eq!(approvals.len(), 1, "{detail}");
    assert_eq!(approvals[0]["action"], "approve");
    assert_eq!(approvals[0]["comment"], "LGTM");
    assert_eq!(approvals[0]["username"], "maint");
    assert!(!approvals[0]["user_id"].as_str().unwrap_or("").is_empty());

    let poll = json_req(&app, "GET", "/api/v1/runners/poll", &runner_jwt(), None).await;
    let poll_body = read_body(poll).await;
    assert_eq!(
        poll_body["data"]["job_id"], job_id,
        "approved job should be pollable: {poll_body}"
    );
}

#[tokio::test]
async fn self_approve_forbidden() {
    let (app, _pool, _state) = test_app().await;
    let admin = admin_jwt();
    let _env_id = create_protected_env(&app, &admin, "locked").await;
    let pid = create_pipeline(&app, &admin, "self-approve-pipe", "locked").await;

    // Maintainer triggers (role >= min_trigger=developer) then tries to self-approve.
    let trig = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pid}/trigger"),
        &maintainer_jwt(),
        Some(serde_json::json!({ "inputs": {}, "environment": "locked" })),
    )
    .await;
    assert_eq!(trig.status(), StatusCode::OK);
    let job_id = read_body(trig).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    let approve = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/approve"),
        &maintainer_jwt(),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(approve.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn platform_admin_may_self_approve() {
    let (app, _pool, _state) = test_app().await;
    let admin = admin_jwt();
    let _env_id = create_protected_env(&app, &admin, "admin-self").await;
    let pid = create_pipeline(&app, &admin, "admin-self-pipe", "admin-self").await;

    let trig = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pid}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "admin-self" })),
    )
    .await;
    assert_eq!(trig.status(), StatusCode::OK);
    let job_id = read_body(trig).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    let approve = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/approve"),
        &admin,
        Some(serde_json::json!({ "comment": "admin override" })),
    )
    .await;
    assert_eq!(approve.status(), StatusCode::OK);
}

#[tokio::test]
async fn automated_trigger_rejected_on_protected_env() {
    let (_app, _pool, state) = test_app().await;
    let admin = admin_jwt();
    // Need HTTP only to seed env + pipeline; reuse app from state via router.
    let app = Router::new().nest("/api/v1", api::router(state.clone()));
    let _env_id = create_protected_env(&app, &admin, "auto-block").await;
    let pid = create_pipeline(&app, &admin, "auto-pipe", "auto-block").await;

    let err = do_trigger(
        &state,
        &pid,
        "webhook",
        &HashMap::new(),
        false,
        false,
        Some("auto-block"),
        None,
        None,
    )
    .await;
    assert!(err.is_err(), "automated trigger must fail");
    let msg = format!("{}", err.unwrap_err());
    assert!(
        msg.contains("protected environment rejects automated"),
        "unexpected error: {msg}"
    );
}

#[tokio::test]
async fn reject_cancels_waiting_job() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let _env_id = create_protected_env(&app, &admin, "reject-env").await;
    let pid = create_pipeline(&app, &admin, "reject-pipe", "reject-env").await;

    let trig = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pid}/trigger"),
        &dev_jwt(),
        Some(serde_json::json!({ "inputs": {}, "environment": "reject-env" })),
    )
    .await;
    let job_id = read_body(trig).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    let reject = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/reject"),
        &maintainer_jwt(),
        Some(serde_json::json!({ "comment": "nope" })),
    )
    .await;
    assert_eq!(reject.status(), StatusCode::OK);
    let body = read_body(reject).await;
    assert_eq!(body["data"]["status"], "cancelled");

    let row = sqlx::query("SELECT status, snapshot FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let status: String = row.get(0);
    let snapshot: String = row.get(1);
    assert_eq!(status, "cancelled");
    let snap: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(snap["approval"]["outcome"], "rejected");
}
