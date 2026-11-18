use std::sync::Arc;

use tokio::sync::RwLock;

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
    /// OIDC sign-in runtime, behind a lock because the settings API rebuilds
    /// it in place (web-saved configuration takes effect without a restart).
    /// `None` inside the lock when SSO is disabled or its configuration is
    /// invalid — local login is unaffected either way.
    pub oidc: Arc<RwLock<Option<Arc<OidcRuntime>>>>,
}

impl AppState {
    /// Snapshot the current runtime: `None` when SSO is disabled.
    pub async fn oidc_runtime(&self) -> Option<Arc<OidcRuntime>> {
        self.oidc.read().await.clone()
    }

    /// Swap the runtime after a settings change (or at startup).
    pub async fn set_oidc_runtime(&self, runtime: Option<Arc<OidcRuntime>>) {
        *self.oidc.write().await = runtime;
    }
}

/// Fixture/test default: SSO absent.
pub fn no_oidc() -> Arc<RwLock<Option<Arc<OidcRuntime>>>> {
    Arc::new(RwLock::new(None))
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
