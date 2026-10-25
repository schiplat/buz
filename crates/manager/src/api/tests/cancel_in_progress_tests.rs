//! Environment deployment concurrency `cancel_in_progress`: a Runner poll in
//! an environment with pending work cancels every older in-flight job
//! (pending / queued / waiting_approval / running) and dispatches only the
//! newest pending job. Includes a 50+ backlog regression proving the candidate
//! window pre-filters "newest per cancel_in_progress environment" BEFORE the
//! `LIMIT 50` — blocked older jobs must not push the newest job out of the
//! window. Other environments are never touched.
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

async fn create_env(app: &Router, jwt: &str, slug: &str, deployment_concurrency: &str) {
    let create = json_req(
        app,
        "POST",
        "/api/v1/environments",
        jwt,
        Some(serde_json::json!({
            "name": slug,
            "slug": slug,
            "protection_enabled": false,
            "deployment_concurrency": deployment_concurrency,
        })),
    )
    .await;
    assert_eq!(create.status(), StatusCode::OK, "create env {slug}");
    let body = read_body(create).await;
    assert_eq!(
        body["data"]["deployment_concurrency"], deployment_concurrency,
        "deployment_concurrency persisted for {slug}"
    );
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
/// marking the running step and an approval-received snapshot.
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
    let update = if status == "running" {
        "UPDATE jobs SET status = ?, step_statuses = ?, runner_id = 'runner-1', \
         start_time = datetime('now') WHERE id = ?"
    } else {
        "UPDATE jobs SET status = ?, step_statuses = ? WHERE id = ?"
    };
    sqlx::query(update)
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

#[tokio::test]
async fn cancel_in_progress_supersedes_older_jobs_and_dispatches_newest() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();

    create_env(&app, &admin, "deploy", "cancel_in_progress").await;
    create_env(&app, &admin, "other", "off").await;
    let pipe_a = create_pipeline(&app, &admin, "deploy-a", "deploy", &["linux-x64"]).await;
    let pipe_b = create_pipeline(&app, &admin, "deploy-b", "deploy", &["linux-x64"]).await;
    // Different labels so the runner never claims it — isolates the
    // "other environment is untouched" assertion from dispatch order.
    let pipe_other = create_pipeline(&app, &admin, "other-pipe", "other", &["gpu-x64"]).await;

    // Older in-flight jobs in the deploy environment, one per state.
    let running_old = trigger_job(&app, &admin, &pipe_a, "deploy").await;
    force_status(
        &pool,
        &running_old,
        "running",
        r#"[{"step_order":0,"status":"running","started_at":"2030-01-01 00:00:00"}]"#,
    )
    .await;
    let queued_old = trigger_job(&app, &admin, &pipe_b, "deploy").await;
    force_status(&pool, &queued_old, "queued", "[]").await;
    let waiting_old = trigger_job(&app, &admin, &pipe_a, "deploy").await;
    force_status(&pool, &waiting_old, "waiting_approval", "[]").await;
    let pending_old = trigger_job(&app, &admin, &pipe_b, "deploy").await;

    // Newest pending job — inserted last so it holds MAX(rowid).
    let newest = trigger_job(&app, &admin, &pipe_a, "deploy").await;
    assert_eq!(job_status(&pool, &newest).await, "pending");

    // Unrelated environment job created before the newest one.
    let other_env_job = trigger_job(&app, &admin, &pipe_other, "other").await;

    let poll = json_req(&app, "GET", "/api/v1/runners/poll", &runner_jwt(), None).await;
    assert_eq!(poll.status(), StatusCode::OK);
    let poll_body = read_body(poll).await;
    assert_eq!(
        poll_body["data"]["job_id"], newest,
        "the newest pending job must be dispatched: {poll_body}"
    );

    // Newest job is now running on the runner.
    let row = sqlx::query("SELECT status, runner_id, start_time FROM jobs WHERE id = ?")
        .bind(&newest)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>(0), "running");
    assert_eq!(row.get::<Option<String>, _>(1).as_deref(), Some("runner-1"));
    assert!(row.get::<Option<String>, _>(2).is_some(), "start_time set");

    // Every older in-flight job in the environment is cancelled.
    for job_id in [&running_old, &queued_old, &waiting_old, &pending_old] {
        assert_eq!(
            job_status(&pool, job_id).await,
            "cancelled",
            "old job {job_id} must be cancelled"
        );
        let end_time: Option<String> = sqlx::query_scalar("SELECT end_time FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(end_time.is_some(), "cancelled job {job_id} gets end_time");
    }

    // Running job's in-flight step was flipped to cancelled with finished_at.
    let step_statuses: String = sqlx::query_scalar("SELECT step_statuses FROM jobs WHERE id = ?")
        .bind(&running_old)
        .fetch_one(&pool)
        .await
        .unwrap();
    let steps: serde_json::Value = serde_json::from_str(&step_statuses).unwrap();
    assert_eq!(steps[0]["status"], "cancelled");
    assert!(steps[0]["finished_at"].is_string());

    // waiting_approval job: snapshot outcome/trail are correct.
    let snapshot_raw: String = sqlx::query_scalar("SELECT snapshot FROM jobs WHERE id = ?")
        .bind(&waiting_old)
        .fetch_one(&pool)
        .await
        .unwrap();
    let snap: serde_json::Value = serde_json::from_str(&snapshot_raw).unwrap();
    assert_eq!(snap["approval"]["outcome"], "cancelled");
    assert_eq!(snap["approval"]["received"], 1, "received count preserved");
    assert_eq!(snap["approval"]["required"], true);
    let trail: Vec<(String, String, String)> =
        sqlx::query("SELECT user_id, username, action FROM job_approvals WHERE job_id = ?")
            .bind(&waiting_old)
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|r: sqlx::sqlite::SqliteRow| (r.get(0), r.get(1), r.get(2)))
            .collect();
    assert_eq!(
        trail,
        vec![(
            "system:environment".to_string(),
            "environment concurrency".to_string(),
            "cancel".to_string(),
        )],
        "cancel_in_progress leaves an audit trail on the waiting job"
    );

    // Other environment is untouched.
    assert_eq!(
        job_status(&pool, &other_env_job).await,
        "pending",
        "jobs in other environments must not be cancelled"
    );
}

#[tokio::test]
async fn cancel_in_progress_backlog_of_50_plus_still_dispatches_newest() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();

    create_env(&app, &admin, "backlog", "cancel_in_progress").await;
    let pid = create_pipeline(&app, &admin, "backlog-pipe", "backlog", &["linux-x64"]).await;
    let env_id: String = sqlx::query_scalar("SELECT id FROM environments WHERE slug = 'backlog'")
        .fetch_one(&pool)
        .await
        .unwrap();

    // 60 blocked pending jobs — more than the candidate window (LIMIT 50).
    for i in 0..60 {
        sqlx::query(
            "INSERT INTO jobs (id, pipeline_id, status, trigger_by, trigger_params, snapshot, \
             environment_id, created_at, updated_at)
             VALUES (?, ?, 'pending', 'manual', '{}', '{\"runner_labels\":[\"linux-x64\"]}', ?, \
             datetime('now'), datetime('now'))",
        )
        .bind(format!("backlog-job-{i:02}"))
        .bind(&pid)
        .bind(&env_id)
        .execute(&pool)
        .await
        .expect("seed backlog job");
    }

    // The newest pending job arrives after the whole backlog (MAX rowid).
    let newest = trigger_job(&app, &admin, &pid, "backlog").await;

    let poll = json_req(&app, "GET", "/api/v1/runners/poll", &runner_jwt(), None).await;
    assert_eq!(poll.status(), StatusCode::OK);
    let poll_body = read_body(poll).await;
    assert_eq!(
        poll_body["data"]["job_id"], newest,
        "newest pending job must be dispatched even with a 60-job backlog: {poll_body}"
    );
    assert_eq!(job_status(&pool, &newest).await, "running");

    // The whole backlog was superseded by cancel_in_progress.
    let cancelled: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE environment_id = ? AND status = 'cancelled'",
    )
    .bind(&env_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cancelled, 60, "all backlog jobs cancelled");
    let still_pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE environment_id = ? AND status = 'pending'",
    )
    .bind(&env_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(still_pending, 0, "no pending stragglers left behind");
}
