use sqlx::SqlitePool;

use crate::config::Config;
use crate::services::log_broadcast::LogBroadcaster;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Config,
    pub log_broadcast: LogBroadcaster,
}

impl AppState {
    /// Backend-neutral handle for `store::` calls. Wraps the SQLite pool
    /// today (an Arc clone, no connection is created); becomes a plain pool
    /// clone when db::init produces a `DbPool` directly (runtime wiring) —
    /// call sites written against this method carry over unchanged.
    pub fn db(&self) -> crate::store::exec::DbPool {
        crate::store::exec::DbPool::from_sqlite(&self.pool)
    }
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
