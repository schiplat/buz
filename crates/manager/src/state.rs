use sqlx::SqlitePool;

use crate::config::Config;
use crate::services::log_broadcast::LogBroadcaster;

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
