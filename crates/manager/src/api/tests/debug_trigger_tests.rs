//! Integration tests for the admin-only "debug (no redaction)" trigger:
//! non-admins get 403 when requesting `debug: true`, admins persist
//! `debug_unredact = 1` on the job. Included via `api/mod.rs` under `#[cfg(test)]`.

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
const MEMBER_ID: &str = "user-1";

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
        clock: Default::default(),
        logging: Default::default(),
    }
}

async fn test_app() -> (Router, SqlitePool) {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

    // Super admin (platform admin).
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

    // Regular member (platform user) with developer role in the default team.
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 0, 0, 1)",
    )
    .bind(MEMBER_ID)
    .bind("dev")
    .bind("dev")
    .execute(&pool)
    .await
    .expect("seed member");

    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')")
        .execute(&pool)
        .await
        .expect("seed team");

    sqlx::query(
        "INSERT INTO team_members (team_id, user_id, role) VALUES ('team-1', ?, 'developer')",
    )
    .bind(MEMBER_ID)
    .execute(&pool)
    .await
    .expect("seed membership");

    let state = AppState {
        pool: pool.clone(),
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    (Router::new().nest("/api/v1", api::router(state)), pool)
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn admin_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(SUPER_ADMIN_ID, Role::Admin, 1)
        .expect("mint admin jwt")
}

fn member_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(MEMBER_ID, Role::User, 0)
        .expect("mint member jwt")
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

const CREATE_PAYLOAD: &str = r#"{
    "name": "debug-trigger",
    "trigger_type": "manual",
    "runner_labels": ["linux-x64"],
    "team_id": "team-1",
    "env": [],
    "steps": [
        { "name": "echo", "run": "echo hi", "uses": "shell" }
    ]
}"#;

async fn create_pipeline(app: &Router) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines")
                .header(AUTHORIZATION, bearer(&admin_jwt()))
                .header("Content-Type", "application/json")
                .body(Body::from(CREATE_PAYLOAD))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "create should succeed");
    let created = read_body(resp).await;
    created["data"]["id"]
        .as_str()
        .expect("pipeline id")
        .to_string()
}

async fn trigger(
    app: &Router,
    jwt: &str,
    pipeline_id: &str,
    debug: bool,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/pipelines/{pipeline_id}/trigger"))
                .header(AUTHORIZATION, bearer(jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(format!(
                    r#"{{"inputs": {{}}, "debug": {debug}}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn job_debug_flag(pool: &SqlitePool, job_id: &str) -> bool {
    let flag: i64 = sqlx::query("SELECT debug_unredact FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("load job")
        .get(0);
    flag != 0
}

#[tokio::test]
async fn member_cannot_request_debug_trigger() {
    let (app, _pool) = test_app().await;
    let pipeline_id = create_pipeline(&app).await;

    let resp = trigger(&app, &member_jwt(), &pipeline_id, true).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "member debug trigger rejected"
    );
    let body = read_body(resp).await;
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("platform admin"),
        "error mentions admin-only: {body}"
    );
}

#[tokio::test]
async fn member_can_trigger_without_debug() {
    let (app, pool) = test_app().await;
    let pipeline_id = create_pipeline(&app).await;

    let resp = trigger(&app, &member_jwt(), &pipeline_id, false).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "member normal trigger succeeds"
    );
    let body = read_body(resp).await;
    let job_id = body["data"]["job_id"].as_str().expect("job id");
    assert!(!job_debug_flag(&pool, job_id).await, "debug flag stays off");
}

#[tokio::test]
async fn admin_debug_trigger_persists_flag() {
    let (app, pool) = test_app().await;
    let pipeline_id = create_pipeline(&app).await;

    let resp = trigger(&app, &admin_jwt(), &pipeline_id, true).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "admin debug trigger succeeds"
    );
    let body = read_body(resp).await;
    let job_id = body["data"]["job_id"].as_str().expect("job id");
    assert!(
        job_debug_flag(&pool, job_id).await,
        "debug_unredact = 1 persisted"
    );
}

#[tokio::test]
async fn admin_normal_trigger_leaves_flag_off() {
    let (app, pool) = test_app().await;
    let pipeline_id = create_pipeline(&app).await;

    let resp = trigger(&app, &admin_jwt(), &pipeline_id, false).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "admin normal trigger succeeds"
    );
    let body = read_body(resp).await;
    let job_id = body["data"]["job_id"].as_str().expect("job id");
    assert!(!job_debug_flag(&pool, job_id).await, "debug flag stays off");
}

async fn get_job(app: &Router, jwt: &str, job_id: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/jobs/{job_id}"))
                .header(AUTHORIZATION, bearer(jwt))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn member_get_one_on_debug_job_is_404() {
    let (app, pool) = test_app().await;
    let pipeline_id = create_pipeline(&app).await;

    let resp = trigger(&app, &admin_jwt(), &pipeline_id, true).await;
    let job_id = read_body(resp).await["data"]["job_id"]
        .as_str()
        .expect("job id")
        .to_string();
    assert!(job_debug_flag(&pool, &job_id).await, "debug job seeded");

    let member_resp = get_job(&app, &member_jwt(), &job_id).await;
    assert_eq!(
        member_resp.status(),
        StatusCode::NOT_FOUND,
        "non-admin cannot even see that a debug job exists (404, not 403)"
    );

    let admin_resp = get_job(&app, &admin_jwt(), &job_id).await;
    assert_eq!(
        admin_resp.status(),
        StatusCode::OK,
        "admin can read debug job metadata"
    );
}

#[tokio::test]
async fn member_get_one_on_normal_job_is_ok() {
    let (app, _pool) = test_app().await;
    let pipeline_id = create_pipeline(&app).await;

    let resp = trigger(&app, &member_jwt(), &pipeline_id, false).await;
    let job_id = read_body(resp).await["data"]["job_id"]
        .as_str()
        .expect("job id")
        .to_string();

    let member_resp = get_job(&app, &member_jwt(), &job_id).await;
    assert_eq!(
        member_resp.status(),
        StatusCode::OK,
        "member can read normal job metadata"
    );
}

async fn list_trigger_events(app: &Router, jwt: &str, pipeline_id: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/pipelines/{pipeline_id}/trigger-events"))
                .header(AUTHORIZATION, bearer(jwt))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    if resp.status() != StatusCode::OK {
        let body = read_body(resp).await;
        panic!("trigger-events list failed: {body}");
    }
    read_body(resp).await
}

#[tokio::test]
async fn trigger_events_hide_debug_job_from_members() {
    let (app, _pool) = test_app().await;
    let pipeline_id = create_pipeline(&app).await;

    let resp = trigger(&app, &admin_jwt(), &pipeline_id, true).await;
    let debug_job_id = read_body(resp).await["data"]["job_id"]
        .as_str()
        .expect("job id")
        .to_string();

    let member_events = list_trigger_events(&app, &member_jwt(), &pipeline_id).await;
    let member_ids: Vec<String> = member_events["data"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| e["job_id"].as_str().map(str::to_string))
        .collect();
    assert!(
        !member_ids.contains(&debug_job_id),
        "debug job trigger event hidden from members: {member_events}"
    );

    let admin_events = list_trigger_events(&app, &admin_jwt(), &pipeline_id).await;
    let admin_ids: Vec<String> = admin_events["data"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| e["job_id"].as_str().map(str::to_string))
        .collect();
    assert!(
        admin_ids.contains(&debug_job_id),
        "debug job trigger event visible to admin: {admin_events}"
    );
}
