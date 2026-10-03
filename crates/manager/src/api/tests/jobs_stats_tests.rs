//! Integration tests for the dashboard job stats endpoints:
//!   GET /jobs/stats/trend  — daily build counts for the last N days
//!   GET /jobs/recent       — lightweight build stream for the Overview sidebar
//! Both must be team-scoped and must hide debug (unredacted) jobs from
//! non-admin users, mirroring the job list/get handlers.
//! Included via `api/mod.rs` under `#[cfg(test)]`.

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};
use sqlx::sqlite::SqlitePoolOptions;
use tower::ServiceExt;

use crate::api;
use crate::config::{AuthConfig, Config, PipelineConfig, RunnerConfig, ServerConfig};
use crate::db::run_migrations;
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const ADMIN_ID: &str = "admin-1";
const USER_ID: &str = "user-1";

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
        clock: Default::default(),
        logging: Default::default(),
    }
}

/// Seed two teams, an admin, a team-1-only developer, pipelines and jobs.
/// Timestamps are computed relative to "now" so the fixed-date assertions stay
/// stable regardless of when the tests run.
async fn test_app() -> Router {
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
    .bind(ADMIN_ID)
    .bind("boss")
    .bind("boss")
    .execute(&pool)
    .await
    .expect("seed admin");

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 0, 0, 1)",
    )
    .bind(USER_ID)
    .bind("dev")
    .bind("dev")
    .execute(&pool)
    .await
    .expect("seed developer");

    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Alpha', 'alpha')")
        .execute(&pool)
        .await
        .expect("seed team-1");
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-2', 'Beta', 'beta')")
        .execute(&pool)
        .await
        .expect("seed team-2");
    sqlx::query(
        "INSERT INTO team_members (team_id, user_id, role) VALUES ('team-1', ?, 'developer')",
    )
    .bind(USER_ID)
    .execute(&pool)
    .await
    .expect("seed membership");

    sqlx::query(
        "INSERT INTO pipelines (id, name, trigger_type, runner_labels, input_defs, is_active, team_id)
         VALUES ('p1', 'alpha-pipe', 'manual', '[]', '[]', 1, 'team-1')",
    )
    .execute(&pool)
    .await
    .expect("seed pipeline p1");
    sqlx::query(
        "INSERT INTO pipelines (id, name, trigger_type, runner_labels, input_defs, is_active, team_id)
         VALUES ('p2', 'beta-pipe', 'manual', '[]', '[]', 1, 'team-2')",
    )
    .execute(&pool)
    .await
    .expect("seed pipeline p2");

    let today = |hh: &str| format!("{} {}", chrono::Utc::now().format("%Y-%m-%d"), hh);
    let yesterday = {
        let d = chrono::Utc::now() - chrono::Duration::days(1);
        format!("{} 10:00:00", d.format("%Y-%m-%d"))
    };

    // team-1 (visible to the developer): 4 today (incl. pending + approval) + 1 yesterday + 1 debug today.
    // team-2 (hidden from the developer): 1 today.
    let jobs: &[(&str, &str, &str, &str, &str, i64)] = &[
        ("j1", "p1", "success", "manual", &today("10:00:00"), 0),
        ("j2", "p1", "success", "webhook", &today("11:00:00"), 0),
        ("j3", "p1", "failed", "cron", &today("12:00:00"), 0),
        ("j4", "p1", "success", "manual", &yesterday, 0),
        ("j5", "p1", "failed", "manual", &today("13:00:00"), 1),
        ("j6", "p2", "success", "manual", &today("14:00:00"), 0),
        ("j7", "p1", "pending", "manual", &today("09:00:00"), 0),
        (
            "j8",
            "p1",
            "waiting_approval",
            "cron",
            &today("08:00:00"),
            0,
        ),
    ];
    for (id, pipeline, status, trigger_by, created_at, debug) in jobs {
        sqlx::query(
            "INSERT INTO jobs (id, pipeline_id, status, trigger_by, created_at, debug_unredact)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(pipeline)
        .bind(status)
        .bind(trigger_by)
        .bind(created_at)
        .bind(debug)
        .execute(&pool)
        .await
        .expect("seed job");
    }

    let state = AppState {
        pool,
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    Router::new().nest("/api/v1", api::router(state))
}

fn bearer(user_id: &str, admin: bool) -> String {
    let role = if admin { Role::Admin } else { Role::User };
    format!(
        "Bearer {}",
        TokenManager::new(JWT_SECRET)
            .create_token(user_id, role, 1)
            .expect("mint jwt")
    )
}

async fn get_json(app: &Router, uri: &str, token: String) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header(AUTHORIZATION, token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    (status, json)
}

fn point_for<'a>(points: &'a [serde_json::Value], date: &str) -> &'a serde_json::Value {
    points
        .iter()
        .find(|p| p["date"].as_str() == Some(date))
        .unwrap_or_else(|| panic!("no trend point for {date}"))
}

fn today_str() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn yesterday_str() -> String {
    (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

#[tokio::test]
async fn admin_trend_includes_all_teams_and_debug_jobs() {
    let app = test_app().await;
    let (status, body) = get_json(
        &app,
        "/api/v1/jobs/stats/trend?days=7",
        bearer(ADMIN_ID, true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], 0);
    assert_eq!(body["data"]["days"], 7);

    let points = body["data"]["points"].as_array().expect("points");
    assert_eq!(points.len(), 7, "zero-filled for every day in range");

    let today = point_for(points, &today_str());
    // 5 team-1 (incl. debug, pending, approval) + 1 team-2 = 7;
    // success = j1,j2,j6 = 3; failed = j3,j5 = 2; pending = j7,j8 = 2
    assert_eq!(today["total"], 7);
    assert_eq!(today["success"], 3);
    assert_eq!(today["failed"], 2);
    assert_eq!(today["cancelled"], 0);
    assert_eq!(today["pending"], 2);
    assert_eq!(today["other"], 0);

    let yesterday = point_for(points, &yesterday_str());
    assert_eq!(yesterday["total"], 1);
    assert_eq!(yesterday["success"], 1);
}

#[tokio::test]
async fn non_admin_trend_is_team_scoped_and_hides_debug_jobs() {
    let app = test_app().await;
    let (status, body) = get_json(
        &app,
        "/api/v1/jobs/stats/trend?days=7",
        bearer(USER_ID, false),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let points = body["data"]["points"].as_array().expect("points");
    let today = point_for(points, &today_str());
    // team-1 only, debug job hidden: success = j1,j2 = 2; failed = j3 = 1; pending = j7,j8 = 2
    assert_eq!(today["total"], 5);
    assert_eq!(today["success"], 2);
    assert_eq!(today["failed"], 1);
    assert_eq!(today["pending"], 2);

    let yesterday = point_for(points, &yesterday_str());
    assert_eq!(yesterday["total"], 1);
    assert_eq!(yesterday["success"], 1);
}

#[tokio::test]
async fn trend_respects_days_range_and_zero_fills() {
    let app = test_app().await;
    // 30-day range must return exactly 30 points even with sparse data.
    let (status, body) = get_json(
        &app,
        "/api/v1/jobs/stats/trend?days=30",
        bearer(ADMIN_ID, true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let points = body["data"]["points"].as_array().expect("points");
    assert_eq!(points.len(), 30);

    // Invalid days clamp to [1, 90] instead of failing.
    let (_, body) = get_json(
        &app,
        "/api/v1/jobs/stats/trend?days=999",
        bearer(ADMIN_ID, true),
    )
    .await;
    assert_eq!(body["data"]["days"], 90);
}

#[tokio::test]
async fn trend_team_param_scopes_for_admin_and_forbids_non_members() {
    let app = test_app().await;

    // Admin scoping to team-1 sees the debug/pending jobs too:
    // total 6 (j1,j2,j3,j5,j7,j8), success 2, failed 2, pending 2.
    let (status, body) = get_json(
        &app,
        &format!("/api/v1/jobs/stats/trend?days=7&team_id=team-1"),
        bearer(ADMIN_ID, true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let today = point_for(
        body["data"]["points"].as_array().expect("points"),
        &today_str(),
    );
    assert_eq!(today["total"], 6);
    assert_eq!(today["success"], 2);
    assert_eq!(today["failed"], 2);
    assert_eq!(today["pending"], 2);

    // Developer asking for another team is rejected.
    let (status, _) = get_json(
        &app,
        &format!("/api/v1/jobs/stats/trend?days=7&team_id=team-2"),
        bearer(USER_ID, false),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn recent_stream_is_lightweight_and_scoped() {
    let app = test_app().await;

    // Admin sees all 8 jobs, newest first (j6 … j4).
    let (status, body) =
        get_json(&app, "/api/v1/jobs/recent?limit=10", bearer(ADMIN_ID, true)).await;
    assert_eq!(status, StatusCode::OK);
    let admin_jobs = body["data"].as_array().expect("recent jobs");
    let ids: Vec<&str> = admin_jobs.iter().filter_map(|j| j["id"].as_str()).collect();
    assert_eq!(ids, vec!["j6", "j5", "j3", "j2", "j1", "j7", "j8", "j4"]);
    let first = &admin_jobs[0];
    assert_eq!(first["pipeline_name"], "beta-pipe");
    assert_eq!(first["status"], "success");
    assert_eq!(first["trigger_by"], "manual");
    assert!(first.get("snapshot").is_none(), "stream stays lightweight");

    // Developer sees only team-1 non-debug jobs, newest first (j3, j2, j1, j7, j8, j4).
    let (status, body) =
        get_json(&app, "/api/v1/jobs/recent?limit=10", bearer(USER_ID, false)).await;
    assert_eq!(status, StatusCode::OK);
    let dev_jobs = body["data"].as_array().expect("recent jobs");
    let ids: Vec<&str> = dev_jobs.iter().filter_map(|j| j["id"].as_str()).collect();
    assert_eq!(ids, vec!["j3", "j2", "j1", "j7", "j8", "j4"]);
    assert!(
        dev_jobs.iter().all(|j| j["team_id"] == "team-1"),
        "developer only sees own team"
    );
}

#[tokio::test]
async fn recent_respects_limit_and_team_param() {
    let app = test_app().await;

    // Admin limited to 2.
    let (_, body) = get_json(&app, "/api/v1/jobs/recent?limit=2", bearer(ADMIN_ID, true)).await;
    let jobs = body["data"].as_array().expect("recent jobs");
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0]["id"], "j6");
    assert_eq!(jobs[1]["id"], "j5");

    // Admin scoping to team-1 hides team-2's j6.
    let (_, body) = get_json(
        &app,
        "/api/v1/jobs/recent?limit=10&team_id=team-1",
        bearer(ADMIN_ID, true),
    )
    .await;
    let jobs = body["data"].as_array().expect("recent jobs");
    let ids: Vec<&str> = jobs.iter().filter_map(|j| j["id"].as_str()).collect();
    assert_eq!(ids, vec!["j5", "j3", "j2", "j1", "j7", "j8", "j4"]);

    // Non-member team still forbidden.
    let (status, _) = get_json(
        &app,
        "/api/v1/jobs/recent?limit=10&team_id=team-2",
        bearer(USER_ID, false),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn trend_with_no_jobs_is_zero_filled() {
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
    .bind(ADMIN_ID)
    .bind("boss")
    .bind("boss")
    .execute(&pool)
    .await
    .expect("seed admin");
    let state = AppState {
        pool,
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    let app = Router::new().nest("/api/v1", api::router(state));

    let (status, body) = get_json(
        &app,
        "/api/v1/jobs/stats/trend?days=7",
        bearer(ADMIN_ID, true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let points = body["data"]["points"].as_array().expect("points");
    assert_eq!(points.len(), 7);
    assert!(points.iter().all(|p| p["total"] == 0), "all days zero");
}
