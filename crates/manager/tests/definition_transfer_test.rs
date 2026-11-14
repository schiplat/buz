//! `pipeline import` / `pipeline export` round-trip: the database is the
//! authority, the directory is a read-only snapshot for git.

mod common;

use manager::store::exec;

const YAML: &str = r#"
name: transfer-demo
environments: [default]
team: default
steps:
  - name: greet
    run: echo hello
"#;

async fn pool() -> exec::DbPool {
    let pool = common::test_db(4).await;
    exec::execute(
        &pool,
        "INSERT INTO teams (id, name, slug) VALUES ('team-default', 'Default', 'default')",
        &[],
    )
    .await
    .unwrap();
    pool
}

#[tokio::test]
async fn import_stores_then_export_round_trips() {
    let pool = pool().await;
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("transfer-demo.yml"), YAML).unwrap();

    let imported = manager::services::definition_transfer::import_definitions(
        &pool,
        src.path(),
        std::path::Path::new("."),
        60,
        false,
    )
    .await
    .unwrap();
    assert_eq!(imported.failed, Vec::<String>::new());
    assert_eq!(imported.stored, 1);

    let stored: String = exec::fetch_optional(
        &pool,
        "SELECT yaml_source FROM pipelines WHERE yaml_path = 'transfer-demo.yml'",
        &[],
    )
    .await
    .unwrap()
    .unwrap()
    .text(0);
    assert_eq!(stored, YAML);

    let out = tempfile::tempdir().unwrap();
    let exported = manager::services::definition_transfer::export_definitions(&pool, out.path())
        .await
        .unwrap();
    assert_eq!(exported.written, 1);
    assert_eq!(
        std::fs::read_to_string(out.path().join("transfer-demo.yml")).unwrap(),
        YAML
    );

    // Exporting again writes nothing — stable diffs in a git working tree.
    let again = manager::services::definition_transfer::export_definitions(&pool, out.path())
        .await
        .unwrap();
    assert_eq!(again.written, 0);
    assert_eq!(again.unchanged, 1);
}

#[tokio::test]
async fn dry_run_reports_without_writing_and_import_never_deletes() {
    let pool = pool().await;
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("transfer-demo.yml"), YAML).unwrap();

    let dry = manager::services::definition_transfer::import_definitions(
        &pool,
        src.path(),
        std::path::Path::new("."),
        60,
        true,
    )
    .await
    .unwrap();
    assert_eq!(dry.stored, 1);
    let rows: i64 = exec::fetch_optional(&pool, "SELECT COUNT(*) FROM pipelines", &[])
        .await
        .unwrap()
        .unwrap()
        .i64(0);
    assert_eq!(rows, 0, "dry run must not write");

    manager::services::definition_transfer::import_definitions(
        &pool,
        src.path(),
        std::path::Path::new("."),
        60,
        false,
    )
    .await
    .unwrap();

    // A second import of an empty directory leaves the stored definition.
    let empty = tempfile::tempdir().unwrap();
    let report = manager::services::definition_transfer::import_definitions(
        &pool,
        empty.path(),
        std::path::Path::new("."),
        60,
        false,
    )
    .await
    .unwrap();
    assert_eq!(report.stored, 0);
    let rows: i64 = exec::fetch_optional(&pool, "SELECT COUNT(*) FROM pipelines", &[])
        .await
        .unwrap()
        .unwrap()
        .i64(0);
    assert_eq!(rows, 1, "import must never deactivate or delete");
}

#[tokio::test]
async fn invalid_file_is_reported_per_file() {
    let pool = pool().await;
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("broken.yml"), "name: [unclosed\n").unwrap();
    std::fs::write(src.path().join("transfer-demo.yml"), YAML).unwrap();

    let report = manager::services::definition_transfer::import_definitions(
        &pool,
        src.path(),
        std::path::Path::new("."),
        60,
        false,
    )
    .await
    .unwrap();
    assert_eq!(report.stored, 1, "the valid file still imports");
    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].starts_with("broken.yml"), "{report:?}");
}
