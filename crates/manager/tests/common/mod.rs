//! Shared harness for Manager integration tests.
//!
//! `TestApp::boot()` builds the real API router over an in-memory SQLite with
//! the standard cast seeded: `admin-1` (platform admin), `ops-1` (platform
//! ops), `user-1` (regular user, developer on `team-1`), and `runner-1`
//! (online, label `linux-x64`). Journeys drive everything else through the
//! public API so the tests exercise exactly what production composes.

// Each integration-test binary includes this module whole; helpers a given
// journey does not use would otherwise trip `-D warnings` dead_code.
#![allow(dead_code)]

use axum::{body::Body, http::header::AUTHORIZATION, http::Request, http::StatusCode, Router};
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use tower::ServiceExt;

use manager::api;
use manager::config::{AuthConfig, Config, RunnerConfig, ServerConfig, StandaloneConfig};
use manager::db::run_migrations;
use manager::services::log_broadcast::LogBroadcaster;
use manager::state::AppState;

pub const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
pub const ADMIN: &str = "admin-1";
pub const OPS: &str = "ops-1";
pub const MEMBER: &str = "user-1";
pub const TEAM: &str = "team-1";
pub const RUNNER: &str = "runner-1";

pub struct TestApp {
    pub router: Router,
    pub pool: SqlitePool,
}

/// Template-DB boot: run the 100+ migrations ONCE per test binary, snapshot
/// the migrated schema with `VACUUM INTO`, then clone that file per test.
/// Boot drops from ~8s per binary to ~milliseconds per test.
static TEMPLATE_DB: tokio::sync::OnceCell<std::path::PathBuf> = tokio::sync::OnceCell::const_new();
static CLONE_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

async fn template_db() -> std::path::PathBuf {
    TEMPLATE_DB
        .get_or_init(|| async {
            // Migrate straight into a file-backed template: VACUUM INTO is a
            // no-op on :memory: sources, so the template must be a real file.
            let path =
                std::env::temp_dir().join(format!("buz-test-template-{}.db", std::process::id()));
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect(&format!("sqlite://{}?mode=rwc", path.display()))
                .await
                .unwrap();
            run_migrations(&pool, JWT_SECRET).await.unwrap();
            pool.close().await;
            path
        })
        .await
        .clone()
}

/// A migrated SQLite pool backed by a private clone of the template file.
/// Each caller gets a fully isolated database.
pub async fn migrated_pool(max_connections: u32) -> SqlitePool {
    let id = CLONE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("buz-test-{}-{id}.db", std::process::id()));
    std::fs::copy(template_db().await, &path).expect("copy template db");
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect(&format!("sqlite://{}", path.display()))
        .await
        .expect("open cloned test db");
    pool
}

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
        pipeline: Default::default(),
        standalone: StandaloneConfig {
            enabled: false,
            auto_approve: vec![],
        },
        plugin_index: Default::default(),
        storage: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
    }
}

impl TestApp {
    /// Router + pool seeded with the standard cast (see module docs).
    pub async fn boot() -> Self {
        let pool = migrated_pool(4).await;
        run_migrations(&pool, JWT_SECRET).await.unwrap();
        for (id, username, admin, ops) in [
            (ADMIN, "boss", 1, 0),
            (OPS, "opsone", 0, 1),
            (MEMBER, "member", 0, 0),
        ] {
            sqlx::query(
                "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_ops, is_active)
                 VALUES (?, ?, 'hash', ?, ?, 0, ?, 1)",
            )
            .bind(id)
            .bind(username)
            .bind(username)
            .bind(admin)
            .bind(ops)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO teams (id, name, slug) VALUES (?, 'Alpha', 'alpha')")
            .bind(TEAM)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO team_members (team_id, user_id, role) VALUES (?, ?, 'developer')")
            .bind(TEAM)
            .bind(MEMBER)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO runners (id, name, register_token, labels, status, max_concurrent_jobs)
             VALUES (?, 'r1', 'tok', '[\"linux-x64\"]', 'online', 4)",
        )
        .bind(RUNNER)
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState {
            pool: pool.clone(),
            config: test_config(),
            log_broadcast: LogBroadcaster::new(),
        };
        Self {
            router: Router::new().nest("/api/v1", api::router(state)),
            pool,
        }
    }

    pub fn jwt(&self, sub: &str, role: Role) -> String {
        TokenManager::new(JWT_SECRET)
            .create_token(sub, role, 24)
            .unwrap()
    }

    pub fn admin_jwt(&self) -> String {
        self.jwt(ADMIN, Role::Admin)
    }

    pub fn ops_jwt(&self) -> String {
        self.jwt(OPS, Role::Ops)
    }

    pub fn member_jwt(&self) -> String {
        self.jwt(MEMBER, Role::User)
    }

    /// One JSON request through the real router; returns (status, parsed body).
    pub async fn call(
        &self,
        method: &str,
        uri: &str,
        bearer: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(b) = bearer {
            builder = builder.header(AUTHORIZATION, format!("Bearer {b}"));
        }
        let req = builder
            .header("Content-Type", "application/json")
            .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
            .unwrap();
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or_default();
        (status, json)
    }

    pub async fn get(&self, uri: &str, bearer: Option<&str>) -> (StatusCode, serde_json::Value) {
        self.call("GET", uri, bearer, None).await
    }

    pub async fn post(
        &self,
        uri: &str,
        bearer: Option<&str>,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        self.call("POST", uri, bearer, Some(body)).await
    }

    pub async fn put(
        &self,
        uri: &str,
        bearer: Option<&str>,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        self.call("PUT", uri, bearer, Some(body)).await
    }

    pub async fn delete(&self, uri: &str, bearer: Option<&str>) -> (StatusCode, serde_json::Value) {
        self.call("DELETE", uri, bearer, None).await
    }
}

/// The `data` envelope of an ApiResponse, or null when absent.
pub fn data(json: &serde_json::Value) -> serde_json::Value {
    json.get("data").cloned().unwrap_or_default()
}
