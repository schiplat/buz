//! Guards that keep team-scoped resources from being silently orphaned:
//! - a secret bound to pipelines cannot change team (the runtime's same-team
//!   join would silently drop it from those pipelines at claim time);
//! - a team with hosts cannot be deleted (`hosts.team_id` has no FK, so the
//!   rows and their stored credentials would otherwise survive invisibly).

mod common;

use axum::http::StatusCode;
use common::TestApp;
use manager::store::{consistency, exec};
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
        "INSERT INTO secrets (id, name, value, team_id)
         VALUES ('s1', 'DEPLOY_KEY', 'x', 'team-1')",
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

#[tokio::test]
async fn consistency_report_flags_legacy_binding_rows() {
    let app = TestApp::boot().await;
    let clean = consistency::check(&app.pool).await.expect("clean check");
    assert!(clean.is_clean(), "fresh fixture must be clean: {clean:?}");

    seed_beta(&app).await;
    for sql in [
        "INSERT INTO pipelines (id, name, team_id) VALUES ('p-orphan', 'orphan-pipe', 'gone')",
        "INSERT INTO pipelines (id, name, team_id) VALUES ('p-empty', 'empty-pipe', '')",
        "INSERT INTO secrets (id, name, value, team_id) VALUES ('s-orphan', 'ORPHAN', 'x', 'gone')",
        "INSERT INTO secrets (id, name, value, team_id) VALUES ('s-empty', 'EMPTY', 'x', '')",
        "INSERT INTO hosts (id, name, host, username, team_id) VALUES ('h-orphan', 'orphan-host', '10.0.0.9', 'root', 'gone')",
        "INSERT INTO pipelines (id, name, team_id) VALUES ('p1', 'alpha-pipe', 'team-1')",
        "INSERT INTO secrets (id, name, value, team_id) VALUES ('s1', 'DEPLOY_KEY', 'x', 'team-1')",
        "INSERT INTO pipeline_secrets (pipeline_id, secret_id) VALUES ('p1', 's1')",
        "UPDATE secrets SET team_id = 'team-2' WHERE id = 's1'",
    ] {
        exec::execute(&app.pool, sql, &[])
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    let report = consistency::check(&app.pool)
        .await
        .expect("consistency check");
    assert_eq!(report.pipelines_missing_team, 1, "{report:?}");
    assert_eq!(report.pipelines_unknown_team, 1, "{report:?}");
    assert_eq!(report.secrets_missing_team, 1, "{report:?}");
    assert_eq!(report.secrets_unknown_team, 1, "{report:?}");
    assert_eq!(report.hosts_unknown_team, 1, "{report:?}");
    assert_eq!(report.cross_team_secret_bindings, 1, "{report:?}");
    assert!(!report.is_clean());
}

#[tokio::test]
async fn normalized_schema_drops_legacy_identity_and_scopes_names() {
    let app = TestApp::boot().await;
    seed_beta(&app).await;

    // The duplicated identity and the misleading column name are gone.
    for legacy in [
        "SELECT owner_group FROM pipelines LIMIT 0",
        "SELECT owner_group FROM secrets LIMIT 0",
        "SELECT team_shared FROM secrets LIMIT 0",
    ] {
        assert!(
            exec::fetch_all(&app.pool, legacy, &[]).await.is_err(),
            "legacy column still present: {legacy}"
        );
    }
    exec::fetch_all(
        &app.pool,
        "SELECT share_with_team FROM secrets LIMIT 0",
        &[],
    )
    .await
    .expect("share_with_team column");

    // Secrets: the same name may exist in two teams, not twice in one.
    for (id, team) in [("sa", "team-1"), ("sb", "team-2")] {
        exec::execute(
            &app.pool,
            "INSERT INTO secrets (id, name, value, team_id) VALUES (?, 'DEPLOY_KEY', 'x', ?)",
            &[id.into(), team.into()],
        )
        .await
        .unwrap_or_else(|e| panic!("{id}: {e}"));
    }
    assert!(
        exec::execute(
            &app.pool,
            "INSERT INTO secrets (id, name, value, team_id) VALUES ('sc', 'DEPLOY_KEY', 'x', 'team-1')",
            &[],
        )
        .await
        .is_err(),
        "duplicate name in one team must be rejected"
    );

    // Hosts: per-team and per-platform-tier uniqueness.
    for (id, team) in [("ha", Some("team-1")), ("hb", Some("team-2"))] {
        exec::execute(
            &app.pool,
            "INSERT INTO hosts (id, name, host, username, team_id) VALUES (?, 'web', '10.0.0.1', 'root', ?)",
            &[id.into(), team.into()],
        )
        .await
        .unwrap_or_else(|e| panic!("{id}: {e}"));
    }
    assert!(
        exec::execute(
            &app.pool,
            "INSERT INTO hosts (id, name, host, username, team_id) VALUES ('hc', 'web', '10.0.0.2', 'root', 'team-1')",
            &[],
        )
        .await
        .is_err(),
        "duplicate host name in one team must be rejected"
    );
    for id in ["hp1", "hp2"] {
        let result = exec::execute(
            &app.pool,
            "INSERT INTO hosts (id, name, host, username, team_id) VALUES (?, 'shared-web', '10.0.0.3', 'root', NULL)",
            &[id.into()],
        )
        .await;
        if id == "hp1" {
            result.expect("first platform host");
        } else {
            assert!(result.is_err(), "platform host names are unique");
        }
    }
}
