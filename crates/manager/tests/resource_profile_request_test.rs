//! Resource-profile requests end to end on the Manager: `runner.resources`
//! frozen into the job, team grants enforced at trigger (fail fast) and again
//! at claim (revocation fails the queued job terminally), and the claim payload
//! carrying the profile name to the Runner.

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use tower::ServiceExt;

use manager::api;
use manager::config::{AuthConfig, Config, PipelineConfig, RunnerConfig, ServerConfig};
use manager::db::run_migrations;
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
const ADMIN_ID: &str = "admin-1";
const MEMBER_ID: &str = "user-1";

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

async fn test_app() -> (Router, SqlitePool) {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    run_migrations(&pool, JWT_SECRET).await.unwrap();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_ops, is_active)
         VALUES ('admin-1', 'boss', 'hash', 'boss', 1, 1, 1, 1),
                ('user-1', 'dev', 'hash', 'dev', 0, 0, 0, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO team_members (team_id, user_id, role) VALUES ('team-1', 'user-1', 'developer')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, max_concurrent_jobs)
         VALUES ('runner-1', 'r1', 'tok', '[\"linux-x64\"]', 'online', 4)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState {
        pool: pool.clone(),
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    (Router::new().nest("/api/v1", api::router(state)), pool)
}

fn jwt(sub: &str, role: Role) -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(sub, role, 1)
        .unwrap()
}

async fn read_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or_default()
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(Body::from(body.unwrap_or("").to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    (status, read_body(resp).await)
}

/// Create a pipeline through the API, then set `resource_profile` the way the
/// YAML scanner does for `runner.resources`.
async fn pipeline_with_profile(app: &Router, pool: &SqlitePool, profile: &str) -> String {
    let (status, created) = call(
        app,
        "POST",
        "/api/v1/pipelines",
        &jwt(ADMIN_ID, Role::Admin),
        Some(
            r#"{
            "name": "profile-trigger",
            "trigger_type": "manual",
            "environments": ["default"],
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "env": [],
            "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
        }"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create: {created}");
    let id = created["data"]["id"].as_str().unwrap().to_string();
    sqlx::query("UPDATE pipelines SET resource_profile = ? WHERE id = ?")
        .bind(profile)
        .bind(&id)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn trigger(app: &Router, pipeline_id: &str) -> (StatusCode, serde_json::Value) {
    call(
        app,
        "POST",
        &format!("/api/v1/pipelines/{pipeline_id}/trigger"),
        &jwt(MEMBER_ID, Role::User),
        Some(r#"{"environment":"default","inputs":{}}"#),
    )
    .await
}

async fn grant(app: &Router, profile: &str) {
    let (status, resp) = call(
        app,
        "PUT",
        "/api/v1/ops/resource-grants",
        &jwt(ADMIN_ID, Role::Admin),
        Some(&format!(r#"{{"team_id":"team-1","profile":"{profile}"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "grant: {resp}");
}

async fn revoke(app: &Router, profile: &str) {
    let (status, _) = call(
        app,
        "DELETE",
        &format!("/api/v1/ops/resource-grants/team-1/{profile}"),
        &jwt(ADMIN_ID, Role::Admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

async fn poll(app: &Router) -> serde_json::Value {
    let (status, body) = call(
        app,
        "GET",
        "/api/v1/runners/poll",
        &jwt("runner-1", Role::Runner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body
}

#[tokio::test]
async fn default_profile_needs_no_grant_and_rides_the_claim_payload_empty() {
    let (app, pool) = test_app().await;
    let pipeline_id = pipeline_with_profile(&app, &pool, "").await;
    let (status, resp) = trigger(&app, &pipeline_id).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let job_id = resp["data"]["job_id"].as_str().unwrap().to_string();
    let stored: String = sqlx::query_scalar("SELECT resource_profile FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, "");

    let body = poll(&app).await;
    assert_eq!(body["data"]["job_id"], job_id);
    assert_eq!(body["data"]["resource_profile"], "");
}

#[tokio::test]
async fn ungranted_profile_fails_at_trigger_before_any_job_exists() {
    let (app, pool) = test_app().await;
    let pipeline_id = pipeline_with_profile(&app, &pool, "large").await;
    let (status, resp) = trigger(&app, &pipeline_id).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{resp}");
    assert!(
        resp["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not authorized to request resource profile 'large'"),
        "{resp}"
    );
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0, "no job may be created for an unauthorized request");
}

#[tokio::test]
async fn granted_profile_is_frozen_into_job_snapshot_and_claim_payload() {
    let (app, pool) = test_app().await;
    let pipeline_id = pipeline_with_profile(&app, &pool, "large").await;
    grant(&app, "large").await;
    // Dispatch requires a sandboxed runner on an enrolled host offering the
    // profile (host mode cannot enforce it — see slot_scheduling_test).
    sqlx::query(
        "UPDATE runners SET execution_mode = 'container', host_id = 'host-a',
         sandbox_instance_id = 'sandboxd-01' WHERE id = 'runner-1'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let doc = serde_json::json!({
        "profiles": { "default": { "memory": "2g", "cpu": "2", "pids": 256, "slots": 1 },
                      "large": { "memory": "8g", "cpu": "4", "pids": 512, "slots": 4 } },
        "tiers": { "container": { "image": "repo@sha256:abc", "runtime": "" } },
        "network": "bridge"
    })
    .to_string();
    sqlx::query(
        "INSERT INTO sandbox_hosts (host_id, instance_id, status, max_resource_slots, desired_version, applied_version)
         VALUES ('host-a', 'sandboxd-01', 'insync', 8, 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sandbox_host_configs (host_id, instance_id, version, document, document_sha256, created_by)
         VALUES ('host-a', 'sandboxd-01', 1, ?, 'sha', 'boss')",
    )
    .bind(&doc)
    .execute(&pool)
    .await
    .unwrap();

    let (status, resp) = trigger(&app, &pipeline_id).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let job_id = resp["data"]["job_id"].as_str().unwrap().to_string();

    let (stored, snapshot_raw): (String, String) =
        sqlx::query_as("SELECT resource_profile, snapshot FROM jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, "large");
    let snapshot: serde_json::Value = serde_json::from_str(&snapshot_raw).unwrap();
    assert_eq!(snapshot["resource_profile"], "large");

    let body = poll(&app).await;
    assert_eq!(body["data"]["job_id"], job_id);
    assert_eq!(body["data"]["resource_profile"], "large");

    // Job detail exposes the request (slots/spec are filled by the scheduling PR).
    let (status, detail) = call(
        &app,
        "GET",
        &format!("/api/v1/jobs/{job_id}"),
        &jwt(ADMIN_ID, Role::Admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["data"]["resource_profile"], "large");
}

#[tokio::test]
async fn revoking_a_grant_while_queued_fails_the_job_at_claim() {
    let (app, pool) = test_app().await;
    let pipeline_id = pipeline_with_profile(&app, &pool, "large").await;
    grant(&app, "large").await;
    let (status, resp) = trigger(&app, &pipeline_id).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let job_id = resp["data"]["job_id"].as_str().unwrap().to_string();

    revoke(&app, "large").await;

    let body = poll(&app).await;
    assert!(
        body["data"].get("job_id").is_none(),
        "revoked job must not be dispatched: {body}"
    );
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        status, "failed",
        "revocation is terminal, not pending forever"
    );
}
