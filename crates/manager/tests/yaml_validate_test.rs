mod common;

use manager::store::exec;
use shared::rbac::TeamRole;

use manager::rbac::AuthUser;
use manager::rbac::TeamMembership;
use manager::services::yaml_validate::validate_pipeline_yaml;
use manager::services::yaml_validate::validate_pipeline_yaml_for_viewer;

fn member_with_default_team() -> AuthUser {
    AuthUser {
        id: "u1".into(),
        username: "dev".into(),
        is_platform_admin: false,
        is_super_admin: false,
        is_ops: false,
        api_token_scope: None,
        memberships: vec![TeamMembership {
            team_id: "t1".into(),
            team_slug: "default".into(),
            team_name: "Default".into(),
            role: TeamRole::Developer,
        }],
    }
}

fn policy(
    local_dir: &std::path::Path,
    require_reviewed: bool,
) -> manager::services::yaml_validate::PluginPolicy {
    manager::services::yaml_validate::PluginPolicy {
        local_dir: local_dir.to_path_buf(),
        reviewed_manifest: local_dir.join("reviews.json"),
        require_reviewed,
    }
}

async fn pool_with_default_team() -> exec::DbPool {
    // The real migrated schema replaces the hand-built mini-DDL fixture.
    let pool = common::test_db(2).await;
    exec::execute(
        &pool,
        "INSERT INTO teams (id, name, slug) VALUES ('t1', 'Default', 'default')",
        &[],
    )
    .await
    .expect("seed default team");
    // `env-default` (slug 'default') already exists via seed_default_rows.
    pool
}

#[tokio::test]
async fn unknown_environment_is_validation_error() {
    let pool = pool_with_default_team().await;
    let yaml = r#"
name: demo
environments: [production]
steps:
  - name: a
    run: echo hi
"#;
    let r = validate_pipeline_yaml(&pool.clone(), yaml)
        .await
        .expect("validate");
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| { e.field == "environments" && e.message.contains("production") }),
        "errors: {:?}",
        r.errors
    );
}

#[tokio::test]
async fn existing_environment_slug_is_valid() {
    let pool = pool_with_default_team().await;
    exec::execute(
        &pool,
        "INSERT INTO environments (id, name, slug) VALUES ('e2', 'Staging', 'staging')",
        &[],
    )
    .await
    .unwrap();
    let yaml = r#"
name: demo
environments: [default, staging]
steps:
  - name: a
    run: echo hi
"#;
    let r = validate_pipeline_yaml(&pool.clone(), yaml)
        .await
        .expect("validate");
    assert!(r.valid, "errors: {:?}", r.errors);
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
    let r = validate_pipeline_yaml(&pool.clone(), yaml)
        .await
        .expect("validate");
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
environments: [default]
steps:
  - name: a
    run: echo hi
"#;
    let r = validate_pipeline_yaml(&pool.clone(), yaml)
        .await
        .expect("validate");
    assert!(r.valid, "errors: {:?}", r.errors);
}

#[tokio::test]
async fn missing_secret_is_validation_error() {
    let pool = pool_with_default_team().await;
    let yaml = r#"
name: demo
environments: [default]
steps:
  - name: push
    engine: shell
    env:
      ACR_TOKEN: ${secrets.ACR_PERSONAL}
    run: echo hi
"#;
    let r = validate_pipeline_yaml(&pool.clone(), yaml)
        .await
        .expect("validate");
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.field == "secrets" && e.message.contains("ACR_PERSONAL")),
        "errors: {:?}",
        r.errors
    );
}

/// The retired `registry_secret` kernel slot is gone from the schema —
/// `deny_unknown_fields` makes it a parse error instead of a purpose slot.
#[tokio::test]
async fn registry_secret_key_is_rejected() {
    let pool = pool_with_default_team().await;
    let yaml = r#"
name: demo
environments: [default]
steps:
  - name: push
    engine: shell
    registry_secret: ${secrets.ACR_PERSONAL}
    run: echo hi
"#;
    let r = validate_pipeline_yaml(&pool.clone(), yaml)
        .await
        .expect("validate");
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.message.contains("registry_secret")),
        "expected unknown-field error, got: {:?}",
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
    let r = validate_pipeline_yaml(&pool.clone(), yaml)
        .await
        .expect("validate");
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
    exec::execute(
        &pool,
        "INSERT INTO teams (id, name, slug) VALUES ('t2', 'Victim', 'victim')",
        &[],
    )
    .await
    .unwrap();
    exec::execute(
        &pool,
        "INSERT INTO secrets (id, name, value, team_id, kind)
         VALUES ('s1', 'REAL_SECRET', 'x', 't2', 'generic')",
        &[],
    )
    .await
    .unwrap();

    let outsider = AuthUser {
        id: "u1".into(),
        username: "dev".into(),
        is_platform_admin: false,
        is_super_admin: false,
        is_ops: false,
        api_token_scope: None,
        memberships: vec![TeamMembership {
            team_id: "t1".into(),
            team_slug: "default".into(),
            team_name: "Default".into(),
            role: TeamRole::Developer,
        }],
    };

    let yaml = r#"
name: probe
environments: [default]
team: victim
steps:
  - name: x
    env:
      T: ${secrets.REAL_SECRET}
    run: echo hi
"#;
    let empty_index = tempfile::tempdir().unwrap();
    let r = validate_pipeline_yaml_for_viewer(
        &pool.clone(),
        yaml,
        &outsider,
        &policy(empty_index.path(), false),
    )
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
        is_ops: false,
        api_token_scope: None,
        memberships: vec![TeamMembership {
            team_id: "t1".into(),
            team_slug: "default".into(),
            team_name: "Default".into(),
            role: TeamRole::Developer,
        }],
    };
    let yaml = r#"
name: demo
environments: [default]
steps:
  - name: push
    engine: shell
    env:
      ACR_TOKEN: ${secrets.MISSING_ACR}
    run: echo hi
"#;
    let empty_index = tempfile::tempdir().unwrap();
    let r = validate_pipeline_yaml_for_viewer(
        &pool.clone(),
        yaml,
        &member,
        &policy(empty_index.path(), false),
    )
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

/// The structural half of validation is content-keyed cached; referential
/// checks must still re-run; deleting a secret has to fail the next
/// validation of unchanged content (trigger-path freshness guarantee).
#[tokio::test]
async fn referential_checks_stay_fresh_for_cached_content() {
    let pool = pool_with_default_team().await;
    exec::execute(
        &pool,
        "INSERT INTO secrets (id, name, value, team_id, kind)
         VALUES ('s1', 'DEP_KEY', 'x', 't1', 'generic')",
        &[],
    )
    .await
    .unwrap();

    let yaml = r#"
name: demo
environments: [default]
steps:
  - name: push
    engine: shell
    env:
      DEP_TOKEN: ${secrets.DEP_KEY}
    run: echo hi
"#;

    let first = validate_pipeline_yaml(&pool.clone(), yaml)
        .await
        .expect("validate");
    assert!(
        first.valid,
        "seeds satisfy referential checks: {:?}",
        first.errors
    );

    // Same content again — now served from the parse cache.
    let second = validate_pipeline_yaml(&pool.clone(), yaml)
        .await
        .expect("validate");
    assert!(second.valid);

    exec::execute(&pool, "DELETE FROM secrets WHERE name = 'DEP_KEY'", &[])
        .await
        .unwrap();

    let third = validate_pipeline_yaml(&pool.clone(), yaml)
        .await
        .expect("validate");
    assert!(!third.valid, "secret deletion must fail unchanged content");
    assert!(
        third.errors.iter().any(|e| e.field == "secrets"),
        "errors: {:?}",
        third.errors
    );
}

#[tokio::test]
async fn viewer_rejects_uses_reference_that_is_not_available() {
    let pool = pool_with_default_team().await;
    let member = member_with_default_team();
    let index = tempfile::tempdir().unwrap();
    let yaml = r#"
name: demo
environments: [default]
steps:
  - name: run
    uses: alice/typo-plugin@1.0.0
"#;
    let r = validate_pipeline_yaml_for_viewer(
        &pool.clone(),
        yaml,
        &member,
        &policy(index.path(), false),
    )
    .await
    .expect("validate");
    assert!(!r.valid);
    let error = r
        .errors
        .iter()
        .find(|e| e.field == "steps.0.uses")
        .unwrap_or_else(|| panic!("expected steps.0.uses error, got {:?}", r.errors));
    assert!(
        error.message.contains("alice/typo-plugin")
            && error.message.contains("is not available here"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn viewer_accepts_local_plugin_and_reports_version_mismatch() {
    let pool = pool_with_default_team().await;
    let member = member_with_default_team();
    let index = tempfile::tempdir().unwrap();
    let plugin_dir = index.path().join("alice").join("hello");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("1.2.0.yaml"),
        "id: alice/hello\nversion: 1.2.0\npublisher: community\nauthor: alice\n\
         source_repo: https://github.com/alice/steps\nengine: shell\ninputs: {}\nrun: echo hi\n",
    )
    .unwrap();

    let ok = r#"
name: demo
environments: [default]
steps:
  - name: run
    uses: alice/hello@1.2.0
"#;
    let r =
        validate_pipeline_yaml_for_viewer(&pool.clone(), ok, &member, &policy(index.path(), false))
            .await
            .expect("validate");
    assert!(
        !r.errors.iter().any(|e| e.field.starts_with("steps.")),
        "available plugin must pass: {:?}",
        r.errors
    );

    // Major pin matches the local 1.2.0 …
    let major = ok.replace("@1.2.0", "@1");
    let r = validate_pipeline_yaml_for_viewer(
        &pool.clone(),
        &major,
        &member,
        &policy(index.path(), false),
    )
    .await
    .expect("validate");
    assert!(!r.errors.iter().any(|e| e.field.starts_with("steps.")));

    // … while a version nobody has must name what IS available.
    let miss = ok.replace("@1.2.0", "@9.9.9");
    let r = validate_pipeline_yaml_for_viewer(
        &pool.clone(),
        &miss,
        &member,
        &policy(index.path(), false),
    )
    .await
    .expect("validate");
    let error = r
        .errors
        .iter()
        .find(|e| e.field == "steps.0.uses")
        .unwrap_or_else(|| panic!("expected version error, got {:?}", r.errors));
    assert!(
        error.message.contains("no version matching @9.9.9") && error.message.contains("1.2.0"),
        "{}",
        error.message
    );
}

/// The strict switch decides severity for the same fact: a resolvable plugin
/// that no one has reviewed. Off → warning, on → error. Availability is never
/// a warning: a reference that cannot resolve fails at save regardless.
#[tokio::test]
async fn unreviewed_plugin_is_a_warning_when_strict_mode_is_off() {
    let pool = pool_with_default_team().await;
    let member = member_with_default_team();
    let index = tempfile::tempdir().unwrap();
    let plugin_dir = index.path().join("alice").join("hello");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("1.0.0.yaml"),
        "id: alice/hello\nversion: 1.0.0\npublisher: community\nauthor: alice\n\
         source_repo: https://github.com/alice/steps\nengine: shell\ninputs: {}\nrun: echo hi\n",
    )
    .unwrap();
    let yaml =
        "name: demo\nenvironments: [default]\nsteps:\n  - name: run\n    uses: alice/hello@1.0.0\n";

    let r = validate_pipeline_yaml_for_viewer(
        &pool.clone(),
        yaml,
        &member,
        &policy(index.path(), false),
    )
    .await
    .expect("validate");
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("steps.0.uses") && w.contains("not in the reviewed manifest")),
        "expected an unreviewed warning, got {:?}",
        r.warnings
    );
    assert!(
        !r.errors
            .iter()
            .any(|e| e.message.contains("reviewed manifest")),
        "strict mode off must not block: {:?}",
        r.errors
    );

    // Same content with the strict switch on → error.
    let r = validate_pipeline_yaml_for_viewer(
        &pool.clone(),
        yaml,
        &member,
        &policy(index.path(), true),
    )
    .await
    .expect("validate");
    assert!(
        r.errors
            .iter()
            .any(|e| e.field == "steps.0.uses" && e.message.contains("review gate")),
        "strict mode must block unreviewed plugins: {:?}",
        r.errors
    );

    // A reviewed hash clears both: the ledger is the switch's input.
    let sha = {
        use sha2::{Digest, Sha256};
        let raw = std::fs::read_to_string(plugin_dir.join("1.0.0.yaml")).unwrap();
        format!("{:x}", Sha256::digest(raw.as_bytes()))
    };
    std::fs::write(
        index.path().join("reviews.json"),
        format!(r#"{{"reviews":[{{"sha256":"{sha}"}}]}}"#),
    )
    .unwrap();
    let r = validate_pipeline_yaml_for_viewer(
        &pool.clone(),
        yaml,
        &member,
        &policy(index.path(), true),
    )
    .await
    .expect("validate");
    assert!(
        !r.errors.iter().any(|e| e.field == "steps.0.uses"),
        "reviewed plugin passes strict mode: {:?}",
        r.errors
    );
}
