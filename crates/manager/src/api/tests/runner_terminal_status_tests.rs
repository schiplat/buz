//! Runner → Manager status reporting must never overwrite a job that already
//! reached a terminal state (`cancelled | success | failed | timeout`):
//! overall status, `end_time`, `retry_times` and step statuses all stick.
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
        plugin_index: Default::default(),
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
        })),
    )
    .await;
    assert_eq!(create.status(), StatusCode::OK, "create env");
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

/// Force a job into the given terminal state with sentinel values that a
/// buggy Runner update would have to clobber for the test to fail.
async fn force_terminal(pool: &SqlitePool, job_id: &str, terminal: &str) {
    sqlx::query(
        "UPDATE jobs SET status = ?, end_time = '2030-01-01 00:00:00', retry_times = 7,
         step_statuses = ? WHERE id = ?",
    )
    .bind(terminal)
    .bind(r#"[{"step_order":0,"status":"success","finished_at":"2030-01-01 00:00:00"}]"#)
    .bind(job_id)
    .execute(pool)
    .await
    .expect("force terminal state");
}

async fn load_job(pool: &SqlitePool, job_id: &str) -> (String, Option<String>, i64, String) {
    let row =
        sqlx::query("SELECT status, end_time, retry_times, step_statuses FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(pool)
            .await
            .expect("load job");
    (row.get(0), row.get(1), row.get(2), row.get(3))
}

#[tokio::test]
async fn terminal_job_silently_ignores_runner_updates() {
    for terminal in ["cancelled", "success", "failed", "timeout"] {
        let (app, pool, _state) = test_app().await;
        let admin = admin_jwt();
        create_env(&app, &admin, "prod").await;
        let pid = create_pipeline(&app, &admin, "term-pipe", "prod").await;
        let job_id = trigger_job(&app, &admin, &pid, "prod").await;
        force_terminal(&pool, &job_id, terminal).await;

        // Runner reports an overall status update — must be accepted (200) but
        // silently ignored so the terminal state sticks.
        let overall = json_req(
            &app,
            "PATCH",
            &format!("/api/v1/jobs/{job_id}/status"),
            &runner_jwt(),
            Some(serde_json::json!({ "status": "running" })),
        )
        .await;
        assert_eq!(
            overall.status(),
            StatusCode::OK,
            "overall update for {terminal}"
        );
        let body = read_body(overall).await;
        assert!(
            body["data"].is_null(),
            "ignored update returns no payload: {body}"
        );

        // Runner reports a per-step update — same silent-ignore contract.
        let step = json_req(
            &app,
            "PATCH",
            &format!("/api/v1/jobs/{job_id}/status"),
            &runner_jwt(),
            Some(serde_json::json!({ "status": "failed", "step_order": 0 })),
        )
        .await;
        assert_eq!(step.status(), StatusCode::OK, "step update for {terminal}");

        let (status, end_time, retry_times, step_statuses) = load_job(&pool, &job_id).await;
        assert_eq!(
            status, terminal,
            "terminal status must survive runner PATCH"
        );
        assert_eq!(
            end_time.as_deref(),
            Some("2030-01-01 00:00:00"),
            "end_time must survive runner PATCH ({terminal})"
        );
        assert_eq!(
            retry_times, 7,
            "retry_times must survive runner PATCH ({terminal})"
        );
        let steps: serde_json::Value = serde_json::from_str(&step_statuses).unwrap();
        assert_eq!(
            steps,
            serde_json::json!([{
                "step_order": 0,
                "status": "success",
                "finished_at": "2030-01-01 00:00:00",
            }]),
            "step statuses must survive runner PATCH ({terminal})"
        );
    }
}

#[tokio::test]
async fn non_terminal_job_still_accepts_runner_updates() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    create_env(&app, &admin, "dev").await;
    let pid = create_pipeline(&app, &admin, "live-pipe", "dev").await;
    let job_id = trigger_job(&app, &admin, &pid, "dev").await;

    // Per-step update while the job is pending → accepted and recorded.
    let step = json_req(
        &app,
        "PATCH",
        &format!("/api/v1/jobs/{job_id}/status"),
        &runner_jwt(),
        Some(serde_json::json!({ "status": "running", "step_order": 0 })),
    )
    .await;
    assert_eq!(step.status(), StatusCode::OK);
    let (_, end_time, retry_times, step_statuses) = load_job(&pool, &job_id).await;
    assert_eq!(end_time, None, "non-terminal step update sets no end_time");
    assert_eq!(retry_times, 0, "step update does not bump retry_times");
    let steps: serde_json::Value = serde_json::from_str(&step_statuses).unwrap();
    assert_eq!(steps[0]["status"], "running");
    assert!(steps[0]["started_at"].is_string(), "started_at recorded");

    // Overall success → status/end_time/retry_times advance, running step
    // stays untouched (only a failed overall report sweeps running steps).
    let overall = json_req(
        &app,
        "PATCH",
        &format!("/api/v1/jobs/{job_id}/status"),
        &runner_jwt(),
        Some(serde_json::json!({ "status": "success" })),
    )
    .await;
    assert_eq!(overall.status(), StatusCode::OK);
    let (status, end_time, retry_times, _) = load_job(&pool, &job_id).await;
    assert_eq!(status, "success");
    assert!(end_time.is_some(), "terminal overall update sets end_time");
    assert_eq!(retry_times, 1, "overall update bumps retry_times once");

    // After reaching the terminal state, further runner updates are ignored.
    let late = json_req(
        &app,
        "PATCH",
        &format!("/api/v1/jobs/{job_id}/status"),
        &runner_jwt(),
        Some(serde_json::json!({ "status": "failed", "step_order": 0 })),
    )
    .await;
    assert_eq!(late.status(), StatusCode::OK);
    let (status, _, retry_times, step_statuses) = load_job(&pool, &job_id).await;
    assert_eq!(status, "success", "late failure must not overwrite success");
    assert_eq!(retry_times, 1, "late failure must not bump retry_times");
    let steps: serde_json::Value = serde_json::from_str(&step_statuses).unwrap();
    assert_eq!(
        steps[0]["status"], "running",
        "late step failure must not overwrite recorded step"
    );
}

#[tokio::test]
async fn failed_overall_report_sweeps_running_steps() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    create_env(&app, &admin, "dev2").await;
    let pid = create_pipeline(&app, &admin, "sweep-pipe", "dev2").await;
    let job_id = trigger_job(&app, &admin, &pid, "dev2").await;

    // Two steps: one finished, one still reported running.
    sqlx::query("UPDATE jobs SET step_statuses = ? WHERE id = ?")
        .bind(
            r#"[{"step_order":0,"status":"success","finished_at":"2030-01-01 00:00:00"},
            {"step_order":1,"status":"running","started_at":"2030-01-01 00:00:01"}]"#,
        )
        .bind(&job_id)
        .execute(&pool)
        .await
        .expect("seed step statuses");

    let overall = json_req(
        &app,
        "PATCH",
        &format!("/api/v1/jobs/{job_id}/status"),
        &runner_jwt(),
        Some(serde_json::json!({ "status": "failed" })),
    )
    .await;
    assert_eq!(overall.status(), StatusCode::OK);

    let (status, _, _, step_statuses) = load_job(&pool, &job_id).await;
    assert_eq!(status, "failed");
    let steps: serde_json::Value = serde_json::from_str(&step_statuses).unwrap();
    assert_eq!(steps[0]["status"], "success", "finished step untouched");
    assert_eq!(steps[1]["status"], "failed", "running step swept to failed");
    assert!(
        steps[1]["finished_at"].is_string(),
        "swept step gets finished_at"
    );
}
