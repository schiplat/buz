use shared::rbac::TeamRole;
use sqlx::SqlitePool;

use crate::rbac::{AuthUser, TeamMembership};
use crate::services::yaml_validate::{validate_pipeline_yaml, validate_pipeline_yaml_for_viewer};

async fn pool_with_default_team() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("memory sqlite");
    sqlx::query(
        "CREATE TABLE teams (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE
        )",
    )
    .execute(&pool)
    .await
    .expect("teams");
    sqlx::query(
        "CREATE TABLE secrets (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            value TEXT NOT NULL,
            team_id TEXT NOT NULL,
            kind TEXT NOT NULL DEFAULT 'generic'
        )",
    )
    .execute(&pool)
    .await
    .expect("secrets");
    sqlx::query(
        "CREATE TABLE platform_vars (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            value TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("platform_vars");
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('t1', 'Default', 'default')")
        .execute(&pool)
        .await
        .expect("seed default team");
    pool
}

#[tokio::test]
async fn unknown_team_is_validation_error() {
    let pool = pool_with_default_team().await;
    let yaml = r#"
name: demo
team: does-not-exist
steps:
  - name: a
    run: echo hi
"#;
    let r = validate_pipeline_yaml(&pool, yaml).await.expect("validate");
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.field == "team" && e.message.contains("does-not-exist")),
        "errors: {:?}",
        r.errors
    );
}

#[tokio::test]
async fn default_team_ok_when_omitted() {
    let pool = pool_with_default_team().await;
    let yaml = r#"
name: demo
steps:
  - name: a
    run: echo hi
"#;
    let r = validate_pipeline_yaml(&pool, yaml).await.expect("validate");
    assert!(r.valid, "errors: {:?}", r.errors);
}

#[tokio::test]
async fn missing_secret_is_validation_error() {
    let pool = pool_with_default_team().await;
    let yaml = r#"
name: demo
steps:
  - name: push
    uses: shell
    registry_secret: ${secrets.ACR_PERSONAL}
    run: echo hi
"#;
    let r = validate_pipeline_yaml(&pool, yaml).await.expect("validate");
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.field == "secrets" && e.message.contains("ACR_PERSONAL")),
        "errors: {:?}",
        r.errors
    );
}

#[tokio::test]
async fn missing_variable_is_validation_error() {
    let pool = pool_with_default_team().await;
    let yaml = r#"
name: demo
variables:
  - BUILD_CACHE_DIR
steps:
  - name: a
    run: echo $BUILD_CACHE_DIR
"#;
    let r = validate_pipeline_yaml(&pool, yaml).await.expect("validate");
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.field == "variables" && e.message.contains("BUILD_CACHE_DIR")),
        "errors: {:?}",
        r.errors
    );
}

#[tokio::test]
async fn viewer_without_team_access_gets_generic_error_no_secret_oracle() {
    let pool = pool_with_default_team().await;
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('t2', 'Victim', 'victim')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO secrets (id, name, value, team_id, kind)
         VALUES ('s1', 'REAL_SECRET', 'x', 't2', 'generic')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let outsider = AuthUser {
        id: "u1".into(),
        username: "dev".into(),
        is_platform_admin: false,
        is_super_admin: false,
        memberships: vec![TeamMembership {
            team_id: "t1".into(),
            team_slug: "default".into(),
            team_name: "Default".into(),
            role: TeamRole::Developer,
        }],
    };

    let yaml = r#"
name: probe
team: victim
steps:
  - name: x
    env:
      T: ${secrets.REAL_SECRET}
    run: echo hi
"#;
    let r = validate_pipeline_yaml_for_viewer(&pool, yaml, &outsider)
        .await
        .expect("validate");
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.field == "team" && e.message.contains("not found or not accessible")),
        "errors: {:?}",
        r.errors
    );
    assert!(
        !r.errors.iter().any(|e| e.message.contains("REAL_SECRET")),
        "must not leak secret names: {:?}",
        r.errors
    );
}

#[tokio::test]
async fn viewer_with_team_access_sees_secret_errors() {
    let pool = pool_with_default_team().await;
    let member = AuthUser {
        id: "u1".into(),
        username: "dev".into(),
        is_platform_admin: false,
        is_super_admin: false,
        memberships: vec![TeamMembership {
            team_id: "t1".into(),
            team_slug: "default".into(),
            team_name: "Default".into(),
            role: TeamRole::Developer,
        }],
    };
    let yaml = r#"
name: demo
steps:
  - name: push
    uses: shell
    registry_secret: ${secrets.MISSING_ACR}
    run: echo hi
"#;
    let r = validate_pipeline_yaml_for_viewer(&pool, yaml, &member)
        .await
        .expect("validate");
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.field == "secrets" && e.message.contains("MISSING_ACR")),
        "errors: {:?}",
        r.errors
    );
}
