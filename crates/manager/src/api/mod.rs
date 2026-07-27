use axum::{Router, routing::{get, post, patch}};
use sqlx::SqlitePool;

use crate::config::Config;
use crate::services::log_broadcast::LogBroadcaster;

mod auth;
pub mod runners;
mod pipelines;
// Re-export helper functions for YAML scanner
pub(crate) use pipelines::{insert_steps, insert_env_vars, insert_secret_refs};
mod hosts;
mod jobs;
mod logs;
mod secrets;
mod variables;

pub fn router(pool: SqlitePool, config: Config) -> Router {
    let app_state = AppState {
        pool,
        config,
        log_broadcast: LogBroadcaster::new(),
    };

    Router::new()
        // Auth (public)
        .route("/auth/login", post(auth::login))
        // Runner endpoints (protected via RunnerClaims extractor in handlers)
        .route("/runners/register", post(runners::register))
        .route("/runners/heartbeat", post(runners::heartbeat))
        .route("/runners/poll", get(runners::poll))
        // Health / debug (public)
        .route("/health", get(|| async { axum::Json(serde_json::json!({"status": "ok"})) }))
        // Admin-protected management routes
        .route("/pipelines", get(pipelines::list).post(pipelines::create))
        .route("/pipelines/validate", post(pipelines::validate_yaml))
        .route("/pipelines/{id}", get(pipelines::get_one).put(pipelines::update).delete(pipelines::delete))
        .route("/pipelines/{id}/toggle-active", patch(pipelines::toggle_active))
        .route("/pipelines/{id}/trigger", post(pipelines::trigger))
        .route("/jobs", get(jobs::list))
        .route("/jobs/{id}", get(jobs::get_one).delete(jobs::delete))
        .route("/jobs/{id}/cancel", post(jobs::cancel))
        .route("/jobs/{id}/status-check", get(jobs::status_check))
        // Runner-protected job routes
        .route("/jobs/{id}/claim", post(jobs::claim))
        .route("/jobs/{id}/status", patch(jobs::update_status))
        .route("/jobs/{id}/logs", post(logs::upload).get(logs::get_logs))
        // WebSocket log stream (admin)
        .route("/jobs/{id}/logs/stream", get(logs::stream_logs))
        // Hosts (admin-protected)
        .route("/hosts", get(hosts::list).post(hosts::create))
        .route("/hosts/{id}", get(hosts::get_one).put(hosts::update).delete(hosts::delete))
        // Secrets (admin-protected) — values are masked in list/get responses
        .route("/secrets", get(secrets::list).post(secrets::create))
        .route("/secrets/{id}", get(secrets::get_one).put(secrets::update).delete(secrets::delete))
        // Variables — platform-scoped configuration
        .route("/variables", get(variables::list).post(variables::create))
        .route("/variables/{id}", get(variables::get_one).put(variables::update).delete(variables::delete))
        // Runner management (admin-protected)
        .route("/runners", get(runners::list_runners).post(runners::create_runner))
        .route("/runners/{id}", get(runners::get_runner).delete(runners::delete_runner))
        .route("/runners/{id}/freeze", patch(runners::toggle_freeze))
        .route("/runners/{id}/events", get(runners::get_events))
        .with_state(app_state)
}

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Config,
    pub log_broadcast: LogBroadcaster,
}

impl axum::extract::FromRef<AppState> for SqlitePool {
    fn from_ref(state: &AppState) -> Self {
        state.pool.clone()
    }
}

impl axum::extract::FromRef<AppState> for Config {
    fn from_ref(state: &AppState) -> Self {
        state.config.clone()
    }
}

impl axum::extract::FromRef<AppState> for LogBroadcaster {
    fn from_ref(state: &AppState) -> Self {
        state.log_broadcast.clone()
    }
}
