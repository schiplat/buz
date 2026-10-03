//! Integration tests for "verbose execution mode": any user (member or admin)
//! may trigger with `verbose: true`. It must persist `verbose_logs = 1` on the
//! job, stay distinct from the admin-only `debug_unredact` flag, and be echoed
//! back to the Runner in the dispatch payload.
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
        step_index: Default::default(),
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

    // Runner with a matching label so dispatch can be exercised.
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

const CREATE_PAYLOAD: &str = r#"{
    "name": "verbose-trigger",
    "trigger_type": "manual",
    "runner_labels": ["linux-x64"],
    "team_id": "team-1",
    "env": [],
    "steps": [
        { "name": "echo", "run": "echo hi", "engine": "shell" }
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
    verbose: bool,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/pipelines/{pipeline_id}/trigger"))
                .header(AUTHORIZATION, bearer(jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(format!(
                    r#"{{"inputs": {{}}, "verbose": {verbose}}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn trigger_debug(app: &Router, jwt: &str, pipeline_id: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/pipelines/{pipeline_id}/trigger"))
                .header(AUTHORIZATION, bearer(jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"inputs": {}, "debug": true}"#))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn job_verbose_flag(pool: &SqlitePool, job_id: &str) -> bool {
    let flag: i64 = sqlx::query("SELECT verbose_logs FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("load job")
        .get(0);
    flag != 0
}

#[tokio::test]
async fn member_can_request_verbose_trigger() {
    let (app, pool) = test_app().await;
    let pipeline_id = create_pipeline(&app).await;

    let resp = trigger(&app, &member_jwt(), &pipeline_id, true).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "member verbose trigger succeeds"
    );
    let body = read_body(resp).await;
    let job_id = body["data"]["job_id"].as_str().expect("job id");
    assert!(
        job_verbose_flag(&pool, job_id).await,
        "verbose_logs = 1 persisted"
    );
    let debug: i64 = sqlx::query("SELECT debug_unredact FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .expect("load job")
        .get(0);
    assert_eq!(debug, 0, "verbose must not flip the admin-only debug flag");
}

#[tokio::test]
async fn member_verbose_off_by_default() {
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
    assert!(
        !job_verbose_flag(&pool, job_id).await,
        "verbose flag stays off"
    );
}

#[tokio::test]
async fn dispatch_echoes_verbose_flag_to_runner() {
    let (app, pool) = test_app().await;
    let pipeline_id = create_pipeline(&app).await;

    let resp = trigger(&app, &member_jwt(), &pipeline_id, true).await;
    assert_eq!(resp.status(), StatusCode::OK, "trigger succeeds");

    let poll = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/runners/poll")
                .header(AUTHORIZATION, bearer(&runner_jwt()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(poll.status(), StatusCode::OK, "poll succeeds");
    let body = read_body(poll).await;

    assert_eq!(
        body["data"]["verbose"], true,
        "dispatch payload carries verbose: {body}"
    );
    assert_eq!(
        body["data"]["debug_unredact"], false,
        "dispatch must not carry debug: {body}"
    );
    drop(pool);
}

/// Debug (unredacted) jobs are invisible to non-admins in every job listing.
#[tokio::test]
async fn member_job_list_hides_debug_jobs() {
    let (app, _pool) = test_app().await;
    let pipeline_id = create_pipeline(&app).await;

    // Admin triggers a debug run.
    let resp = trigger_debug(&app, &admin_jwt(), &pipeline_id).await;
    assert_eq!(resp.status(), StatusCode::OK, "debug trigger succeeds");
    let body = read_body(resp).await;
    let job_id = body["data"]["job_id"].as_str().expect("job id").to_string();

    // Admin sees it, with the debug flag set.
    let admin_list = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/jobs")
                .header(AUTHORIZATION, bearer(&admin_jwt()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(admin_list.status(), StatusCode::OK);
    let admin_body = read_body(admin_list).await;
    let admin_job = admin_body["data"]
        .as_array()
        .expect("jobs array")
        .iter()
        .find(|j| j["id"].as_str() == Some(&job_id))
        .expect("admin sees the debug job");
    assert_eq!(admin_job["debug_unredact"], true, "flag exposed to admin");

    // Non-admin must not see the record at all.
    let member_list = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/jobs")
                .header(AUTHORIZATION, bearer(&member_jwt()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(member_list.status(), StatusCode::OK);
    let member_body = read_body(member_list).await;
    let ids: Vec<&str> = member_body["data"]
        .as_array()
        .expect("jobs array")
        .iter()
        .filter_map(|j| j["id"].as_str())
        .collect();
    assert!(
        !ids.contains(&job_id.as_str()),
        "member job list must hide debug job, got: {ids:?}"
    );
}

/// Pipeline cards must not surface a debug run as the "latest job" for non-admins.
#[tokio::test]
async fn member_pipeline_list_hides_debug_latest_job() {
    let (app, _pool) = test_app().await;
    let pipeline_id = create_pipeline(&app).await;

    // Admin triggers a debug run so it becomes the pipeline's latest job.
    let resp = trigger_debug(&app, &admin_jwt(), &pipeline_id).await;
    assert_eq!(resp.status(), StatusCode::OK, "debug trigger succeeds");

    // Admin sees the debug job as latest.
    let admin_list = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/pipelines")
                .header(AUTHORIZATION, bearer(&admin_jwt()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let admin_body = read_body(admin_list).await;
    let admin_pipe = admin_body["data"]
        .as_array()
        .expect("pipelines array")
        .iter()
        .find(|p| p["id"].as_str() == Some(&pipeline_id))
        .expect("pipeline listed for admin");
    assert!(
        admin_pipe["latest_job_id"].as_str().is_some(),
        "admin sees latest job: {admin_pipe}"
    );

    // Non-admin: latest_job is null and run totals exclude the debug job.
    let member_list = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/pipelines")
                .header(AUTHORIZATION, bearer(&member_jwt()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(member_list.status(), StatusCode::OK);
    let member_body = read_body(member_list).await;
    let member_pipe = member_body["data"]
        .as_array()
        .expect("pipelines array")
        .iter()
        .find(|p| p["id"].as_str() == Some(&pipeline_id))
        .expect("pipeline listed for member");
    assert!(
        member_pipe["latest_job_id"].is_null(),
        "member must not see the debug latest job: {member_pipe}"
    );
    assert_eq!(
        member_pipe["job_total"], 0,
        "member run totals exclude debug job: {member_pipe}"
    );
}
