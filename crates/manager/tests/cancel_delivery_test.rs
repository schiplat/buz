//! Cancel delivery over the claim channel: cancellations (operator cancel /
//! pipeline disable / cancel_in_progress supersede) ride on every
//! `/runners/poll` response via the one-shot `cancel_ack_pending` flag,
//! replacing the runner's former per-job 2s status-check probes.

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
use manager::config::AuthConfig;
use manager::config::Config;
use manager::config::PipelineConfig;
use manager::config::RunnerConfig;
use manager::config::ServerConfig;
use manager::db::run_migrations;
use manager::services::log_broadcast::LogBroadcaster;
use manager::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const SUPER_ADMIN_ID: &str = "admin-1";

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
        pipeline: isolated_pipeline_config(),
        standalone: Default::default(),
        plugin_index: Default::default(),
        storage: Default::default(),
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

    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')")
        .execute(&pool)
        .await
        .expect("seed team");
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-2', 'Other', 'other')")
        .execute(&pool)
        .await
        .expect("seed other team");

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

fn admin_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(SUPER_ADMIN_ID, Role::Admin, 1)
        .expect("mint admin jwt")
}

fn runner_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token("runner-1", Role::Runner, 1)
        .expect("mint runner jwt")
}

async fn negotiated_poll(app: &Router, ack: Option<&str>) -> axum::response::Response {
    let mut builder = Request::builder()
        .method("GET")
        .uri("/api/v1/runners/poll")
        .header(AUTHORIZATION, format!("Bearer {}", runner_jwt()))
        .header("prefer", "wait=45");
    if let Some(ack) = ack {
        builder = builder.header("x-buz-cancel-ack", ack);
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
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
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {jwt}"));
    let request = match body {
        Some(payload) => builder
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    app.clone().oneshot(request).await.unwrap()
}

async fn create_pipeline(app: &Router, jwt: &str, name: &str) -> String {
    create_pipeline_in_team(app, jwt, name, "team-1").await
}

async fn create_pipeline_in_team(app: &Router, jwt: &str, name: &str, team_id: &str) -> String {
    let pipe = json_req(
        app,
        "POST",
        "/api/v1/pipelines",
        jwt,
        Some(serde_json::json!({
            "name": name,
            "trigger_type": "manual",
            "runner_labels": ["linux-x64"],
            "team_id": team_id,
            "environments": ["default"],
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

#[tokio::test]
async fn poll_delivers_operator_cancel_once_and_clears_the_flag() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let pipe = create_pipeline(&app, &admin, "cancel-delivery").await;

    let trig = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipe}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "default" })),
    )
    .await;
    let job_id = read_body(trig).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    sqlx::query(
        "UPDATE jobs SET status = 'running', runner_id = 'runner-1', \
         start_time = datetime('now') WHERE id = ?",
    )
    .bind(&job_id)
    .execute(&pool)
    .await
    .expect("force running");

    let cancel = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/cancel"),
        &admin,
        None,
    )
    .await;
    assert_eq!(cancel.status(), StatusCode::OK, "cancel job");

    // First poll delivers the notice.
    let poll = negotiated_poll(&app, None).await;
    assert_eq!(poll.status(), StatusCode::OK);
    let body = read_body(poll).await;
    let delivered = body["data"]["cancelled_jobs"]
        .as_array()
        .expect("cancelled_jobs array")
        .clone();
    assert!(
        delivered
            .iter()
            .any(|v| v.as_str() == Some(job_id.as_str())),
        "first poll delivers: {body}"
    );

    // Unacked notices repeat (response-loss retry semantics).
    let poll2 = negotiated_poll(&app, None).await;
    let body2 = read_body(poll2).await;
    assert!(
        body2["data"]["cancelled_jobs"]
            .as_array()
            .map(|a| a.iter().any(|v| v.as_str() == Some(job_id.as_str())))
            .unwrap_or(false),
        "unacked notice repeats: {body2}"
    );

    // Ack clears the durable pending flag. Queue fresh work so the same poll
    // can return immediately with a claim instead of entering another wait.
    let pipe2 = create_pipeline(&app, &admin, "cancel-ack-followup").await;
    let followup = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipe2}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "default" })),
    )
    .await;
    let followup_id = read_body(followup).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let poll3 = negotiated_poll(&app, Some(&job_id)).await;
    let body3 = read_body(poll3).await;
    assert_eq!(
        body3["data"]["job_id"], followup_id,
        "new job is claimed: {body3}"
    );
    assert!(
        body3["data"]["cancelled_jobs"]
            .as_array()
            .unwrap()
            .is_empty(),
        "acked notice is not repeated: {body3}"
    );
    let pending: i64 = sqlx::query_scalar("SELECT cancel_ack_pending FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pending, 0);
}

#[tokio::test]
async fn poll_delivers_cancels_even_at_capacity() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let target_pipe = create_pipeline(&app, &admin, "capacity-cancel-target").await;
    let busy_pipe = create_pipeline(&app, &admin, "capacity-cancel-busy").await;

    // Capacity 1 with one *uncancelled* running job → every poll hits the
    // capacity gate, while a second runner-owned cancellation must be delivered.
    sqlx::query("UPDATE runners SET max_concurrent_jobs = 1 WHERE id = 'runner-1'")
        .execute(&pool)
        .await
        .expect("cap runner");

    async fn running_job(app: &Router, pool: &SqlitePool, admin: &str, pipe: &str) -> String {
        let trig = json_req(
            app,
            "POST",
            &format!("/api/v1/pipelines/{pipe}/trigger"),
            admin,
            Some(serde_json::json!({ "inputs": {}, "environment": "default" })),
        )
        .await;
        let job_id = read_body(trig).await["data"]["job_id"]
            .as_str()
            .unwrap()
            .to_string();
        sqlx::query(
            "UPDATE jobs SET status = 'running', runner_id = 'runner-1', \
             start_time = datetime('now') WHERE id = ?",
        )
        .bind(&job_id)
        .execute(pool)
        .await
        .expect("force running");
        job_id
    }

    let _busy_job = running_job(&app, &pool, &admin, &busy_pipe).await;
    let job_id = running_job(&app, &pool, &admin, &target_pipe).await;

    let cancel = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/cancel"),
        &admin,
        None,
    )
    .await;
    assert_eq!(cancel.status(), StatusCode::OK, "cancel job");

    let poll = negotiated_poll(&app, None).await;
    assert_eq!(poll.status(), StatusCode::OK);
    let body = read_body(poll).await;
    assert!(
        body["data"]["job_id"].is_null(),
        "capacity gate must not dispatch: {body}"
    );
    assert!(
        body["data"]["cancelled_jobs"]
            .as_array()
            .map(|a| a.iter().any(|v| v.as_str() == Some(job_id.as_str())))
            .unwrap_or(false),
        "cancel notice rides the capacity-gated poll: {body}"
    );
}

#[tokio::test]
async fn claim_response_carries_cancel_notices_alongside_the_job() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let pipe_a = create_pipeline(&app, &admin, "claim-cancel-a").await;
    let pipe_b = create_pipeline(&app, &admin, "claim-cancel-b").await;

    // One running job that gets cancelled, plus one fresh pending job.
    let trig_a = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipe_a}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "default" })),
    )
    .await;
    let running = read_body(trig_a).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    sqlx::query(
        "UPDATE jobs SET status = 'running', runner_id = 'runner-1', \
         start_time = datetime('now') WHERE id = ?",
    )
    .bind(&running)
    .execute(&pool)
    .await
    .expect("force running");
    let cancel = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{running}/cancel"),
        &admin,
        None,
    )
    .await;
    assert_eq!(cancel.status(), StatusCode::OK, "cancel running job");

    let trig_b = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipe_b}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "default" })),
    )
    .await;
    let pending = read_body(trig_b).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    let poll = negotiated_poll(&app, None).await;
    assert_eq!(poll.status(), StatusCode::OK);
    let body = read_body(poll).await;
    assert_eq!(
        body["data"]["job_id"].as_str(),
        Some(pending.as_str()),
        "pending job is claimed: {body}"
    );
    assert!(
        body["data"]["cancelled_jobs"]
            .as_array()
            .map(|a| a.iter().any(|v| v.as_str() == Some(running.as_str())))
            .unwrap_or(false),
        "cancel notice rides the claim response: {body}"
    );
}

#[tokio::test]
async fn negotiated_long_poll_wakes_when_a_new_job_is_committed() {
    let (app, _pool, state) = test_app().await;
    let admin = admin_jwt();
    let pipe = create_pipeline(&app, &admin, "long-poll-wakeup").await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let app_for_poll = app.clone();
    let state_for_poll = state.clone();

    let poll_task = tokio::spawn(async move {
        let _ = started_tx.send(());
        let auth = manager::middleware::RunnerClaims {
            runner_id: "runner-1".into(),
        };
        let response = manager::api::runners::poll_with_wait(
            auth,
            state_for_poll,
            0,
            Vec::new(),
            std::time::Duration::from_secs(2),
        )
        .await
        .expect("long poll");
        let body = read_body(response.into_response()).await;
        body
    });
    started_rx.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let trigger = json_req(
        &app_for_poll,
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

    let body = tokio::time::timeout(std::time::Duration::from_millis(500), poll_task)
        .await
        .expect("poll should wake immediately, not wait for its timeout")
        .unwrap();
    assert_eq!(body["data"]["job_id"], job_id);
}

#[tokio::test]
async fn claim_poll_delivers_cancel_notices_without_legacy_compat_mode() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let pipe = create_pipeline(&app, &admin, "legacy-cancel").await;
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
    sqlx::query(
        "UPDATE jobs SET status = 'running', runner_id = 'runner-1', \
         start_time = datetime('now') WHERE id = ?",
    )
    .bind(&job_id)
    .execute(&pool)
    .await
    .unwrap();
    let cancel = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/cancel"),
        &admin,
        None,
    )
    .await;
    assert_eq!(cancel.status(), StatusCode::OK);

    let poll = json_req(&app, "GET", "/api/v1/runners/poll", &runner_jwt(), None).await;
    let body = read_body(poll).await;
    assert!(
        body["data"]["cancelled_jobs"]
            .as_array()
            .map(|ids| ids.iter().any(|id| id.as_str() == Some(job_id.as_str())))
            .unwrap_or(false),
        "claim channel delivers cancellation notice: {body}"
    );
    let pending: i64 = sqlx::query_scalar("SELECT cancel_ack_pending FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        pending, 1,
        "legacy poll must not consume new-protocol notice"
    );
}

use axum::response::IntoResponse;

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

#[tokio::test]
async fn negotiated_long_poll_wakes_when_a_running_job_is_cancelled() {
    let (app, pool, state) = test_app().await;
    let admin = admin_jwt();
    let pipe = create_pipeline(&app, &admin, "cancel-wakeup").await;
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
    sqlx::query(
        "UPDATE jobs SET status = 'running', runner_id = 'runner-1', \
         start_time = datetime('now') WHERE id = ?",
    )
    .bind(&job_id)
    .execute(&pool)
    .await
    .unwrap();

    // A separate pipeline avoids the cancelled job's same-pipeline capacity gate.
    let _other = create_pipeline(&app, &admin, "cancel-wakeup-other").await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let state_for_poll = state.clone();
    let poll_task = tokio::spawn(async move {
        let _ = started_tx.send(());
        manager::api::runners::poll_with_wait(
            manager::middleware::RunnerClaims {
                runner_id: "runner-1".into(),
            },
            state_for_poll,
            0,
            Vec::new(),
            std::time::Duration::from_secs(2),
        )
        .await
        .expect("cancel long poll")
    });
    started_rx.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let cancel = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/cancel"),
        &admin,
        None,
    )
    .await;
    assert_eq!(cancel.status(), StatusCode::OK);
    let response = tokio::time::timeout(std::time::Duration::from_millis(500), poll_task)
        .await
        .expect("cancel should wake the poll")
        .unwrap();
    let body = read_body(response.into_response()).await;
    assert!(
        body["data"]["cancelled_jobs"]
            .as_array()
            .map(|a| a.iter().any(|v| v.as_str() == Some(job_id.as_str())))
            .unwrap_or(false),
        "poll carries cancelled job immediately: {body}"
    );
}

#[tokio::test]
async fn unrelated_wakeup_returns_retry_hint_to_refresh_active_count() {
    let (app, pool, state) = test_app().await;
    let admin = admin_jwt();
    let busy_pipe = create_pipeline(&app, &admin, "busy-active-snapshot").await;
    let other_pipe = create_pipeline(&app, &admin, "other-runner-event").await;
    sqlx::query("UPDATE runners SET max_concurrent_jobs = 1 WHERE id = 'runner-1'")
        .execute(&pool)
        .await
        .unwrap();

    let running = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{busy_pipe}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "default" })),
    )
    .await;
    let running_id = read_body(running).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    sqlx::query(
        "UPDATE jobs SET status = 'running', runner_id = 'runner-1', \
         start_time = datetime('now') WHERE id = ?",
    )
    .bind(&running_id)
    .execute(&pool)
    .await
    .unwrap();

    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let poll_state = state.clone();
    let poll_task = tokio::spawn(async move {
        let _ = started_tx.send(());
        manager::api::runners::poll_with_wait(
            manager::middleware::RunnerClaims {
                runner_id: "runner-1".into(),
            },
            poll_state,
            1,
            Vec::new(),
            std::time::Duration::from_secs(2),
        )
        .await
        .expect("capacity long poll")
    });
    started_rx.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // This event wakes the long poll, but its job is blocked by the stale
    // active_jobs=1 snapshot. The poll must not claim beyond capacity; it
    // remains parked until its normal deadline for the Runner to refresh.
    let trigger = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{other_pipe}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "default" })),
    )
    .await;
    assert_eq!(trigger.status(), StatusCode::OK);
    let response = tokio::time::timeout(std::time::Duration::from_secs(3), poll_task)
        .await
        .expect("poll returns at its deadline after unrelated wakes")
        .unwrap();
    let body = read_body(response.into_response()).await;
    assert!(
        body["data"]["job_id"].is_null(),
        "no claim with stale capacity: {body}"
    );
}

#[tokio::test]
async fn bound_runner_only_claims_its_frozen_team_and_shared_runner_can_claim_any_team() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let team1_pipe = create_pipeline_in_team(&app, &admin, "team-one-pipe", "team-1").await;
    let team2_pipe = create_pipeline_in_team(&app, &admin, "team-two-pipe", "team-2").await;

    sqlx::query("UPDATE runners SET team_id = 'team-1' WHERE id = 'runner-1'")
        .execute(&pool)
        .await
        .unwrap();

    let foreign = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{team2_pipe}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "default" })),
    )
    .await;
    let foreign_id = read_body(foreign).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let own = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{team1_pipe}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "default" })),
    )
    .await;
    let own_id = read_body(own).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    let poll = negotiated_poll(&app, None).await;
    let body = read_body(poll).await;
    assert_eq!(
        body["data"]["job_id"], own_id,
        "bound runner claims its team: {body}"
    );
    let foreign_status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(&foreign_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        foreign_status, "pending",
        "foreign team job remains pending"
    );

    // Remove the first claim and make the runner shared; the same foreign job
    // becomes eligible without relying on a label convention.
    sqlx::query("UPDATE jobs SET status = 'success' WHERE id = ?")
        .bind(own_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE runners SET team_id = NULL WHERE id = 'runner-1'")
        .execute(&pool)
        .await
        .unwrap();
    let poll = negotiated_poll(&app, None).await;
    let body = read_body(poll).await;
    assert_eq!(
        body["data"]["job_id"], foreign_id,
        "shared runner claims cross-team job: {body}"
    );
}

#[tokio::test]
async fn bound_runner_skips_legacy_job_without_frozen_team() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let pipe = create_pipeline_in_team(&app, &admin, "legacy-team-pipe", "team-1").await;
    sqlx::query("UPDATE runners SET team_id = 'team-1' WHERE id = 'runner-1'")
        .execute(&pool)
        .await
        .unwrap();
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
    sqlx::query("UPDATE jobs SET snapshot = json_remove(snapshot, '$.team_id') WHERE id = ?")
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();

    let poll = json_req(&app, "GET", "/api/v1/runners/poll", &runner_jwt(), None).await;
    let body = read_body(poll).await;
    assert!(
        body["data"]["job_id"].is_null(),
        "bound runner skips unknown legacy ownership: {body}"
    );
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "pending");
}

#[tokio::test]
async fn runner_team_binding_patch_distinguishes_omitted_bound_and_shared() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let bind = json_req(
        &app,
        "PUT",
        "/api/v1/runners/runner-1",
        &admin,
        Some(serde_json::json!({ "team_id": "team-2" })),
    )
    .await;
    assert_eq!(bind.status(), StatusCode::OK);
    assert_eq!(read_body(bind).await["data"]["team_id"], "team-2");

    let keep = json_req(
        &app,
        "PUT",
        "/api/v1/runners/runner-1",
        &admin,
        Some(serde_json::json!({ "labels": ["linux-x64"] })),
    )
    .await;
    assert_eq!(keep.status(), StatusCode::OK);
    assert_eq!(read_body(keep).await["data"]["team_id"], "team-2");

    let shared = json_req(
        &app,
        "PUT",
        "/api/v1/runners/runner-1",
        &admin,
        Some(serde_json::json!({ "team_id": null })),
    )
    .await;
    assert_eq!(shared.status(), StatusCode::OK);
    assert!(read_body(shared).await["data"]["team_id"].is_null());

    let team_id: Option<String> =
        sqlx::query_scalar("SELECT team_id FROM runners WHERE id = 'runner-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(team_id, None);
}

#[tokio::test]
async fn runner_team_binding_cannot_change_with_running_jobs() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let pipe = create_pipeline(&app, &admin, "runner-binding-running").await;
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
    sqlx::query("UPDATE jobs SET status='running', runner_id='runner-1' WHERE id=?")
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap();

    let update = json_req(
        &app,
        "PUT",
        "/api/v1/runners/runner-1",
        &admin,
        Some(serde_json::json!({ "team_id": "team-1" })),
    )
    .await;
    assert_eq!(update.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn team_with_bound_runner_cannot_be_deleted() {
    let (app, pool, _state) = test_app().await;
    sqlx::query("UPDATE runners SET team_id='team-2' WHERE id='runner-1'")
        .execute(&pool)
        .await
        .unwrap();
    let response = json_req(&app, "DELETE", "/api/v1/teams/team-2", &admin_jwt(), None).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM teams WHERE id='team-2'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(exists, 1);
}

#[tokio::test]
async fn migrated_legacy_runner_remains_shared_by_default() {
    let (_app, pool, _state) = test_app().await;
    let team_id: Option<String> =
        sqlx::query_scalar("SELECT team_id FROM runners WHERE id='runner-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        team_id, None,
        "migration must not silently bind legacy runners"
    );
}

#[tokio::test]
async fn claim_does_not_resolve_foreign_team_ssh_host_credentials() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    sqlx::query(
        "INSERT INTO hosts (id, name, host, port, username, credential_type, credential_data, team_id) \
         VALUES ('host-foreign', 'foreign-host', '192.0.2.40', 22, 'deploy', 'vault', 'foreign-secret', 'team-2')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let pipeline = json_req(
        &app,
        "POST",
        "/api/v1/pipelines",
        &admin,
        Some(serde_json::json!({
            "name": "foreign-host-pipeline",
            "trigger_type": "manual",
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "environments": ["default"],
            "env": [],
            "steps": [{ "name": "remote", "run": "echo hi", "engine": "ssh", "host": "foreign-host" }]
        })),
    )
    .await;
    assert_eq!(pipeline.status(), StatusCode::OK, "create pipeline");
    let pipeline_id = read_body(pipeline).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let trigger = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipeline_id}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "default" })),
    )
    .await;
    assert_eq!(trigger.status(), StatusCode::OK);

    let poll = json_req(&app, "GET", "/api/v1/runners/poll", &runner_jwt(), None).await;
    let body = read_body(poll).await;
    let step = &body["data"]["tasks"][0];
    assert!(
        step["host_resolution_error"]
            .as_str()
            .is_some_and(|error| error.contains("not available to this pipeline team")),
        "foreign host should be rejected without revealing its details: {body}"
    );
    assert!(
        step.get("host").is_none(),
        "foreign host connection details/credentials must not be attached: {step}"
    );
    assert!(
        !body.to_string().contains("foreign-secret"),
        "foreign host credential must not reach claim payload"
    );
}

#[tokio::test]
async fn foreign_team_backlog_does_not_fill_claim_limit() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let foreign_pipe = create_pipeline_in_team(&app, &admin, "foreign-backlog", "team-2").await;
    let own_pipe = create_pipeline_in_team(&app, &admin, "own-after-backlog", "team-1").await;
    sqlx::query("UPDATE runners SET team_id = 'team-1' WHERE id = 'runner-1'")
        .execute(&pool)
        .await
        .unwrap();

    for index in 0..55 {
        sqlx::query(
            "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot, created_at) \
             VALUES (?, ?, 'pending', 'manual', ?, datetime('now', ?))",
        )
        .bind(format!("foreign-{index:02}"))
        .bind(&foreign_pipe)
        .bind(r#"{"team_id":"team-2","tasks":[]}"#)
        .bind(format!("+{index} seconds"))
        .execute(&pool)
        .await
        .unwrap();
    }
    let own_trigger = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{own_pipe}/trigger"),
        &admin,
        Some(serde_json::json!({ "inputs": {}, "environment": "default" })),
    )
    .await;
    let own_job = read_body(own_trigger).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    let poll = json_req(&app, "GET", "/api/v1/runners/poll", &runner_jwt(), None).await;
    let body = read_body(poll).await;
    assert_eq!(
        body["data"]["job_id"], own_job,
        "eligible job survives foreign backlog: {body}"
    );
}

#[tokio::test]
async fn runner_execution_mode_requires_fresh_supported_capability() {
    use shared::protocol::{ExecutionBackendStatus, RunnerExecutionCapabilities, SystemInfo};
    let (app, _pool, _state) = test_app().await;
    let admin = admin_jwt();

    let unknown = json_req(
        &app,
        "PUT",
        "/api/v1/runners/runner-1",
        &admin,
        Some(serde_json::json!({ "execution_mode": "container" })),
    )
    .await;
    assert_eq!(unknown.status(), StatusCode::CONFLICT);

    let info = SystemInfo {
        runner_version: "test".into(),
        hostname: "runner-1".into(),
        os: "Linux".into(),
        kernel_version: "test".into(),
        cpu_arch: "x86_64".into(),
        cpu_cores: 1,
        cpu_model: "test".into(),
        cpu_usage_percent: 0.0,
        memory_total_mb: 1024,
        memory_used_mb: 1,
        swap_total_mb: 0,
        swap_used_mb: 0,
        disk_total_gb: 1.0,
        disk_used_gb: 0.0,
        network_interfaces: vec![],
        ip_addresses: vec![],
        uptime_seconds: 1,
        execution_capabilities: Some(RunnerExecutionCapabilities {
            container: ExecutionBackendStatus::Supported,
            container_reason: None,
            sandbox: ExecutionBackendStatus::Unknown,
            sandbox_reason: None,
            vm: ExecutionBackendStatus::Unsupported,
            vm_reason: Some("KVM unavailable".into()),
            runtimes: Default::default(),
            checked_at: Some(chrono::Utc::now().to_rfc3339()),
        }),
        sandboxd: None,
    };
    let heartbeat = json_req(
        &app,
        "POST",
        "/api/v1/runners/heartbeat",
        &runner_jwt(),
        Some(serde_json::json!({ "status": "online", "system_info": info })),
    )
    .await;
    assert_eq!(heartbeat.status(), StatusCode::OK);

    let allowed = json_req(
        &app,
        "PUT",
        "/api/v1/runners/runner-1",
        &admin,
        Some(serde_json::json!({ "execution_mode": "container" })),
    )
    .await;
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(
        read_body(allowed).await["data"]["execution_mode"],
        "container"
    );

    let denied_vm = json_req(
        &app,
        "PUT",
        "/api/v1/runners/runner-1",
        &admin,
        Some(serde_json::json!({ "execution_mode": "vm" })),
    )
    .await;
    assert_eq!(denied_vm.status(), StatusCode::CONFLICT);

    let mut stale_info = info.clone();
    stale_info
        .execution_capabilities
        .as_mut()
        .unwrap()
        .checked_at = Some((chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339());
    let stale = json_req(
        &app,
        "POST",
        "/api/v1/runners/heartbeat",
        &runner_jwt(),
        Some(serde_json::json!({ "status": "online", "system_info": stale_info })),
    )
    .await;
    assert_eq!(stale.status(), StatusCode::OK);
    let denied_stale = json_req(
        &app,
        "PUT",
        "/api/v1/runners/runner-1",
        &admin,
        Some(serde_json::json!({ "execution_mode": "vm" })),
    )
    .await;
    assert_eq!(denied_stale.status(), StatusCode::CONFLICT);
}
