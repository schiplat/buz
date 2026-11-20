//! Guards that keep team-scoped resources from being silently orphaned:
//! - a secret bound to pipelines cannot change team (the runtime's same-team
//!   join would silently drop it from those pipelines at claim time);
//! - a team with hosts cannot be deleted (`hosts.team_id` has no FK, so the
//!   rows and their stored credentials would otherwise survive invisibly).

mod common;

use axum::http::StatusCode;
use common::TestApp;
use manager::store::exec;
use serde_json::json;

async fn seed_beta(app: &TestApp) {
    exec::execute(
        &app.pool,
        "INSERT INTO teams (id, name, slug) VALUES ('team-2', 'Beta', 'beta')",
        &[],
    )
    .await
    .expect("seed team-2");
}

async fn secret_team(app: &TestApp, id: &str) -> String {
    exec::fetch_optional(
        &app.pool,
        "SELECT team_id FROM secrets WHERE id = ?",
        &[id.into()],
    )
    .await
    .expect("read secret team")
    .expect("secret row")
    .text(0)
}

#[tokio::test]
async fn secret_move_is_refused_while_bound_to_a_pipeline() {
    let app = TestApp::boot().await;
    seed_beta(&app).await;
    for sql in [
        "INSERT INTO secrets (id, name, value, owner_group, team_id)
         VALUES ('s1', 'DEPLOY_KEY', 'x', 'alpha', 'team-1')",
        "INSERT INTO pipelines (id, name, team_id) VALUES ('p1', 'alpha-pipe', 'team-1')",
        "INSERT INTO pipeline_secrets (pipeline_id, secret_id) VALUES ('p1', 's1')",
    ] {
        exec::execute(&app.pool, sql, &[])
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    let admin = app.admin_jwt();

    let (status, resp) = app
        .put(
            "/api/v1/secrets/s1",
            Some(&admin),
            json!({ "value": "", "team_id": "team-2" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{resp}");
    assert!(
        resp.to_string().contains("alpha-pipe"),
        "guard must name the bound pipeline: {resp}"
    );
    assert_eq!(
        secret_team(&app, "s1").await,
        "team-1",
        "secret must not move while bound"
    );

    // Once unbound, the same move is allowed.
    exec::execute(
        &app.pool,
        "DELETE FROM pipeline_secrets WHERE secret_id = 's1'",
        &[],
    )
    .await
    .unwrap();
    let (status, resp) = app
        .put(
            "/api/v1/secrets/s1",
            Some(&admin),
            json!({ "value": "", "team_id": "team-2" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(secret_team(&app, "s1").await, "team-2");
}

#[tokio::test]
async fn team_delete_is_refused_while_hosts_remain() {
    let app = TestApp::boot().await;
    seed_beta(&app).await;
    exec::execute(
        &app.pool,
        "INSERT INTO hosts (id, name, host, username, team_id)
         VALUES ('h1', 'beta-host', '10.0.0.9', 'root', 'team-2')",
        &[],
    )
    .await
    .unwrap();
    let admin = app.admin_jwt();

    let (status, resp) = app.delete("/api/v1/teams/team-2", Some(&admin)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{resp}");
    assert!(
        resp.to_string().contains("host"),
        "delete guard must report the hosts: {resp}"
    );

    exec::execute(&app.pool, "DELETE FROM hosts WHERE id = 'h1'", &[])
        .await
        .unwrap();
    let (status, resp) = app.delete("/api/v1/teams/team-2", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
}
