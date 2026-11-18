use std::sync::Arc;

use crate::config::Config;
use crate::services::log_broadcast::LogBroadcaster;
use crate::services::oidc::OidcRuntime;
use crate::store::exec::DbPool;

#[derive(Clone)]
pub struct AppState {
    /// Backend-neutral pool: SQLite (single mode) or Postgres (multi target).
    pub pool: DbPool,
    pub config: Config,
    pub log_broadcast: LogBroadcaster,
    /// OIDC sign-in runtime. `None` when SSO is disabled or its configuration
    /// is invalid — local login is unaffected either way.
    pub oidc: Option<Arc<OidcRuntime>>,
}

impl AppState {
    /// Backend-neutral handle for `store::` calls — a plain pool clone.
    /// Kept as a method so older call sites written against `state.db()`
    /// keep working; new code may use `&state.pool` directly.
    pub fn db(&self) -> DbPool {
        self.pool.clone()
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
