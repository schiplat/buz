use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use shared::version::VERSION;
use sqlx::sqlite::SqlitePoolOptions;

use super::{health, mark_started, HealthQuery};
use crate::config::{AuthConfig, Config, PipelineConfig, RunnerConfig, ServerConfig};
use crate::db::run_migrations;
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";

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
        logging: Default::default(),
    }
}

async fn test_state() -> AppState {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");
    AppState {
        pool,
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    }
}

#[tokio::test]
async fn health_includes_core_fields_and_liveness() {
    mark_started();
    let state = test_state().await;
    let (code, Json(body)) = health(State(state), Query(HealthQuery::default())).await;

    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], VERSION);
    assert_eq!(body["name"], "Buz");
    assert_eq!(body["component"], "manager");
    assert!(body["uptime_seconds"].as_i64().is_some(), "uptime present");
    assert!(body["started_at"].is_string(), "started_at present");
}

#[tokio::test]
async fn health_reports_database_jobs_and_ws_signals() {
    let state = test_state().await;

    // Seed a backlog signal: one running + one waiting_approval job.
    sqlx::query(
        "INSERT INTO pipelines (id, name, trigger_type, runner_labels, input_defs, is_active, team_id)
         VALUES ('p1', 'h-probe', 'manual', '[]', '[]', 1, '')",
    )
    .execute(&state.pool)
    .await
    .expect("seed pipeline");
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by) VALUES ('j1', 'p1', 'running', 'manual')",
    )
    .execute(&state.pool)
    .await
    .expect("seed running job");
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by) VALUES ('j2', 'p1', 'waiting_approval', 'cron')",
    )
    .execute(&state.pool)
    .await
    .expect("seed approval job");

    // Simulate an active log stream for the running job.
    state.log_broadcast.subscribe("j1");

    let (code, Json(body)) = health(State(state), Query(HealthQuery::default())).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["database"]["status"], "ok");
    assert!(
        body["database"]["latency_ms"].as_f64().is_some(),
        "latency measured"
    );
    assert!(
        body["database"]["pool"]["max"].as_u64().unwrap() >= 1,
        "pool max reported"
    );
    assert!(body["database"]["pool"]["in_use"].as_u64().is_some());
    assert_eq!(body["jobs"]["running"], 1);
    assert_eq!(body["jobs"]["waiting_approval"], 1);
    assert_eq!(body["jobs"]["pending"], 0);
    assert_eq!(body["ws"]["job_sinks"], 1);
    assert_eq!(body["ws"]["pipeline_sinks"], 0);
}

#[tokio::test]
async fn health_degraded_with_503_when_db_unavailable_in_readiness_mode() {
    let state = test_state().await;
    state.pool.close().await;

    // Readiness probe: DB down → 503 + degraded.
    let (code, Json(body)) = health(
        State(state),
        Query(HealthQuery {
            readiness: Some(true),
        }),
    )
    .await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "degraded");
    assert_eq!(body["database"]["status"], "error");
}
