use manager::services::pipeline_scanner::filename_from_pipeline_name;
use manager::services::pipeline_scanner::lock_yaml_file;
use manager::services::pipeline_scanner::normalize_yaml_path_key;
use manager::services::pipeline_scanner::read_yaml_for_scan;
use manager::services::pipeline_scanner::upsert_from_yaml;
use manager::services::pipeline_scanner::validate_yaml_filename;
use manager::services::pipeline_scanner::yaml_content_etag;
use manager::services::pipeline_scanner::yaml_path_storage;
use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;

#[tokio::test]
async fn scanner_projects_automatic_trigger_environments_on_insert_and_update() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("memory sqlite");
    manager::db::run_migrations(&pool, "test-secret-0123456789-abcdefghijklmnop")
        .await
        .expect("migrate");
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-default', 'Default', 'default')")
        .execute(&pool)
        .await
        .expect("seed default team");

    let cron_yaml = r#"
name: scanner-projection
team: default
environments: [default, staging, production]
on:
  cron:
    schedule: "0 2 * * *"
    environment: production
steps:
  - name: run
    run: echo cron
"#;
    let parsed = serde_yaml::from_str(cron_yaml).expect("parse cron YAML");
    let pipeline_id = upsert_from_yaml(
        &pool,
        "scanner-projection.yml",
        &parsed,
        Path::new("."),
        cron_yaml,
        60,
    )
    .await
    .expect("insert projection");

    let projected: (String, String, String) = sqlx::query_as(
        "SELECT webhook_environment, cron_environment, environments FROM pipelines WHERE id = ?",
    )
    .bind(&pipeline_id)
    .fetch_one(&pool)
    .await
    .expect("read cron projection");
    assert_eq!(projected.0, "");
    assert_eq!(projected.1, "production");
    assert_eq!(projected.2, r#"["default","staging","production"]"#);

    sqlx::query("UPDATE pipelines SET cron_environment = '' WHERE id = ?")
        .bind(&pipeline_id)
        .execute(&pool)
        .await
        .expect("simulate stale projection");
    let parsed = serde_yaml::from_str(cron_yaml).expect("parse same cron YAML");
    let same_id = upsert_from_yaml(
        &pool,
        "scanner-projection.yml",
        &parsed,
        Path::new("."),
        cron_yaml,
        60,
    )
    .await
    .expect("repair same-hash projection");
    assert_eq!(same_id, pipeline_id);
    let cron_environment: String =
        sqlx::query_scalar("SELECT cron_environment FROM pipelines WHERE id = ?")
            .bind(&pipeline_id)
            .fetch_one(&pool)
            .await
            .expect("read repaired projection");
    assert_eq!(cron_environment, "production");

    let webhook_yaml = cron_yaml
        .replace(
            "  cron:\n    schedule: \"0 2 * * *\"\n    environment: production",
            "  webhook:\n    path: deploy\n    environment: staging",
        )
        .replace("echo cron", "echo webhook");
    let parsed = serde_yaml::from_str(&webhook_yaml).expect("parse webhook YAML");
    let updated_id = upsert_from_yaml(
        &pool,
        "scanner-projection.yml",
        &parsed,
        Path::new("."),
        &webhook_yaml,
        60,
    )
    .await
    .expect("update projection");
    assert_eq!(updated_id, pipeline_id);
    let projected: (String, String) =
        sqlx::query_as("SELECT webhook_environment, cron_environment FROM pipelines WHERE id = ?")
            .bind(&pipeline_id)
            .fetch_one(&pool)
            .await
            .expect("read webhook projection");
    assert_eq!(projected, ("staging".into(), "".into()));
}

#[tokio::test]
async fn scanner_read_waits_for_file_lock_and_observes_replaced_content() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scan_dir = dir.path().to_string_lossy().to_string();
    let filename = "scanner-lock.yml";
    let initial = "name: initial\nsteps: []\n";
    let replacement = "name: replacement\nsteps: []\n";
    std::fs::write(dir.path().join(filename), initial).expect("write initial YAML");

    let guard = lock_yaml_file(&scan_dir, filename)
        .await
        .expect("lock YAML file");
    let scan_dir_for_task = scan_dir.clone();
    let read_task =
        tokio::spawn(async move { read_yaml_for_scan(&scan_dir_for_task, filename).await });

    tokio::task::yield_now().await;
    std::fs::write(dir.path().join(filename), replacement).expect("replace YAML");
    drop(guard);

    let (read, _guard) = read_task.await.expect("scanner task").expect("read YAML");
    assert_eq!(read, replacement);
}

#[test]
fn rejects_path_separators_and_traversal() {
    assert!(validate_yaml_filename("../evil.yml").is_err());
    assert!(validate_yaml_filename("sub/evil.yml").is_err());
    assert!(validate_yaml_filename("evil\\x.yml").is_err());
    assert!(validate_yaml_filename("").is_err());
    assert!(validate_yaml_filename("noext").is_err());
}

#[test]
fn accepts_safe_basenames() {
    assert!(validate_yaml_filename("hello-world.yml").is_ok());
    assert!(validate_yaml_filename("Hello_World_2.yaml").is_ok());
}

#[test]
fn rejects_dots_in_stem() {
    assert!(validate_yaml_filename("Hello_World.2.yaml").is_err());
    assert!(validate_yaml_filename("a.b.yml").is_err());
    assert!(validate_yaml_filename("sd-fad.-sf.yaml").is_err());
}

#[test]
fn slugifies_pipeline_name() {
    assert_eq!(
        filename_from_pipeline_name("Hello World"),
        "hello-world.yml"
    );
    assert_eq!(filename_from_pipeline_name("  "), "pipeline.yml");
    assert_eq!(filename_from_pipeline_name("sd -fad. sf"), "sd-fad-sf.yml");
}

#[test]
fn storage_path_is_basename_only() {
    let p = yaml_path_storage("./pipelines", "demo.yml").expect("path");
    assert_eq!(p, "demo.yml");
}

#[test]
fn normalize_strips_directories() {
    assert_eq!(
        normalize_yaml_path_key("/data/pipelines/demo.yml"),
        "demo.yml"
    );
    assert_eq!(normalize_yaml_path_key("./pipelines/demo.yml"), "demo.yml");
    assert_eq!(normalize_yaml_path_key("demo.yml"), "demo.yml");
}

#[test]
fn etag_stable_for_same_content() {
    let a = yaml_content_etag("name: x\nsteps: []\n");
    let b = yaml_content_etag("name: x\nsteps: []\n");
    assert_eq!(a, b);
    assert_ne!(a, yaml_content_etag("name: y\nsteps: []\n"));
}

#[tokio::test]
async fn scanner_projects_runner_resources_into_resource_profile_on_insert_and_update() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("memory sqlite");
    manager::db::run_migrations(&pool, "test-secret-0123456789-abcdefghijklmnop")
        .await
        .expect("migrate");
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-default', 'Default', 'default')")
        .execute(&pool)
        .await
        .expect("seed default team");

    let large_yaml = r#"
name: scanner-resources
team: default
environments: [default]
runner:
  labels: [linux-x64]
  resources: large
steps:
  - name: run
    run: make
"#;
    let parsed = serde_yaml::from_str(large_yaml).expect("parse YAML");
    let pipeline_id = upsert_from_yaml(
        &pool,
        "scanner-resources.yml",
        &parsed,
        Path::new("."),
        large_yaml,
        60,
    )
    .await
    .expect("insert");
    let profile: String = sqlx::query_scalar("SELECT resource_profile FROM pipelines WHERE id = ?")
        .bind(&pipeline_id)
        .fetch_one(&pool)
        .await
        .expect("read profile");
    assert_eq!(profile, "large");

    // Explicit `default` and an omitted key both normalize to '' (implicit).
    let default_yaml = large_yaml.replace("resources: large", "resources: default");
    let parsed = serde_yaml::from_str(&default_yaml).expect("parse YAML");
    let same_id = upsert_from_yaml(
        &pool,
        "scanner-resources.yml",
        &parsed,
        Path::new("."),
        &default_yaml,
        60,
    )
    .await
    .expect("update");
    assert_eq!(same_id, pipeline_id);
    let profile: String = sqlx::query_scalar("SELECT resource_profile FROM pipelines WHERE id = ?")
        .bind(&pipeline_id)
        .fetch_one(&pool)
        .await
        .expect("read profile");
    assert_eq!(
        profile, "",
        "explicit default normalizes to the implicit form"
    );
}
