use super::{expand_uses_step, local_step_index_dir, resolve_uses_steps, LocalStepIndex};
use shared::protocol::StepDef;
use std::collections::BTreeMap;
use std::path::Path;

fn write_spec(dir: &Path, file: &str, yaml: &str) {
    std::fs::write(dir.join(file), yaml).unwrap();
}

fn feishu_spec(id: &str, version: &str) -> String {
    format!(
        r#"
id: {id}
name: Feishu notify
version: {version}
engine: shell
default_timeout: 30s
inputs:
  webhook:
    type: string
    required: true
    masking: true
run: |
  curl -fsSL -X POST "$INPUT_WEBHOOK" -d '{{}}'
"#
    )
}

fn seed_index() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_spec(
        dir.path(),
        "feishu-1.2.0.yaml",
        &feishu_spec("notify/feishu", "1.2.0"),
    );
    write_spec(
        dir.path(),
        "feishu-1.4.0.yaml",
        &feishu_spec("notify/feishu", "1.4.0"),
    );
    write_spec(dir.path(), "broken.yaml", "id: [broken");
    dir
}

fn uses_step(uses: &str) -> StepDef {
    StepDef {
        name: "notify".into(),
        run: String::new(),
        engine: None,
        uses: Some(uses.into()),
        with: None,
        image: None,
        volumes: None,
        host: None,
        host_labels: None,
        hosts_strategy: None,
        dir: None,
        timeout: None,
        retry: None,
        continue_on_error: None,
        secrets: None,
        env: None,
        privileged: None,
        registry_secret: None,
        config: None,
        config_source: None,
        config_key_path: None,
        config_dest: None,
        config_render: None,
    }
}

#[test]
fn local_index_dir_defaults_under_scan_dir() {
    let dir = local_step_index_dir("/opt/buz/pipelines", "");
    assert_eq!(dir, Path::new("/opt/buz/pipelines/steps"));
    let configured = local_step_index_dir("/opt/buz/pipelines", "/etc/buz/steps");
    assert_eq!(configured, Path::new("/etc/buz/steps"));
}

#[test]
fn resolve_major_pin_picks_highest_version() {
    let dir = seed_index();
    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let resolved = index.resolve("notify/feishu@1", "1.5.6").unwrap();
    assert_eq!(resolved.spec.version, "1.4.0");

    let resolved = index.resolve("notify/feishu@1.2.0", "1.5.6").unwrap();
    assert_eq!(resolved.spec.version, "1.2.0");
    assert!(resolved.source.starts_with("local:"));
    assert_eq!(resolved.content_sha256.len(), 64);
}

#[test]
fn resolve_by_content_sha256() {
    let dir = seed_index();
    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let any = index.resolve("notify/feishu@1.2.0", "1.5.6").unwrap();
    let by_hash = index
        .resolve(
            &format!("notify/feishu@sha256:{}", any.content_sha256),
            "1.5.6",
        )
        .unwrap();
    assert_eq!(by_hash.spec.version, "1.2.0");

    let wrong = format!("notify/feishu@sha256:{}", "0".repeat(64));
    assert!(index.resolve(&wrong, "1.5.6").is_err());
}

#[test]
fn resolve_reports_missing_and_stale_requires_buz() {
    let dir = seed_index();
    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let err = index.resolve("notify/absent@1", "1.5.6").unwrap_err();
    assert!(err.contains("not found in local step index"), "{err}");

    let dir = tempfile::tempdir().unwrap();
    write_spec(
        dir.path(),
        "future.yaml",
        &feishu_spec("notify/feishu", "9.0.0").replacen(
            "engine: shell",
            "requires_buz: \">=99.0.0\"\nengine: shell",
            1,
        ),
    );
    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let err = index.resolve("notify/feishu@9", "1.5.6").unwrap_err();
    assert!(err.contains("too new"), "{err}");
}

#[test]
fn expand_maps_with_to_input_env_and_clears_uses() {
    let dir = seed_index();
    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let resolved = index.resolve("notify/feishu@1.2.0", "1.5.6").unwrap();

    let mut step = uses_step("notify/feishu@1.2.0");
    let mut with = BTreeMap::new();
    with.insert("webhook".to_string(), "${secrets.FEISHU}".to_string());
    step.with = Some(with);

    let expanded = expand_uses_step(&step, &resolved).unwrap();
    assert_eq!(expanded.engine.as_deref(), Some("shell"));
    assert!(expanded.run.contains("INPUT_WEBHOOK"));
    assert_eq!(expanded.uses, None, "expanded step is concrete");
    assert_eq!(expanded.with, None);
    let env = expanded.env.unwrap();
    assert_eq!(
        env.get("INPUT_WEBHOOK").map(String::as_str),
        Some("${secrets.FEISHU}")
    );
    assert_eq!(
        expanded.timeout.as_deref(),
        Some("30s"),
        "template default_timeout applies"
    );
}

#[test]
fn expand_rejects_unknown_and_missing_required_inputs() {
    let dir = seed_index();
    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let resolved = index.resolve("notify/feishu@1.2.0", "1.5.6").unwrap();

    let mut step = uses_step("notify/feishu@1.2.0");
    let mut with = BTreeMap::new();
    with.insert("nope".to_string(), "x".to_string());
    step.with = Some(with);
    let errors = expand_uses_step(&step, &resolved).unwrap_err();
    assert!(
        errors.iter().any(|e| e.contains("does not declare")),
        "{errors:?}"
    );

    let step = uses_step("notify/feishu@1.2.0");
    let errors = expand_uses_step(&step, &resolved).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("missing required input 'webhook'")),
        "{errors:?}"
    );
}

#[tokio::test]
async fn reviewed_manifest_flags_hash_and_strict_mode_rejects_unreviewed() {
    let dir = seed_index();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    crate::db::run_migrations(&pool, "test-secret-0123456789abcdef")
        .await
        .unwrap();

    // Discover the 1.4.0 content hash via a lenient resolution.
    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let resolved = index.resolve("notify/feishu@1", "1.5.6").unwrap();

    let manifest_path = dir.path().join("reviews.json");
    std::fs::write(
        &manifest_path,
        format!(
            r#"{{"reviews":[{{"id":"notify/feishu","version":"1.4.0","sha256":"{}"}}]}}"#,
            resolved.content_sha256
        ),
    )
    .unwrap();

    let policy = super::ResolutionPolicy {
        reviewed_hashes: super::load_reviewed_hashes(manifest_path.to_str().unwrap()),
        require_reviewed: true,
    };

    // Reviewed hash → resolves under strict mode, provenance flagged.
    let mut step = uses_step("notify/feishu@1.4.0");
    let mut with = BTreeMap::new();
    with.insert("webhook".to_string(), "https://hook".to_string());
    step.with = Some(with);
    let out = resolve_uses_steps(&pool, "pipe-r", vec![step], dir.path(), &policy)
        .await
        .unwrap();
    assert_eq!(out.len(), 1);
    let reviewed: i64 =
        sqlx::query_scalar("SELECT reviewed FROM step_resolutions WHERE pipeline_id = 'pipe-r'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reviewed, 1);

    // Different (unreviewed) pin → strict mode rejects.
    let err = resolve_uses_steps(
        &pool,
        "pipe-r2",
        vec![uses_step("notify/feishu@1.2.0")],
        dir.path(),
        &policy,
    )
    .await
    .unwrap_err();
    assert!(err.contains("not officially reviewed"), "{err}");

    // Lenient policy allows it (warning only) and records reviewed=0.
    let mut step = uses_step("notify/feishu@1.2.0");
    let mut with = BTreeMap::new();
    with.insert("webhook".to_string(), "https://hook".to_string());
    step.with = Some(with);
    let out = resolve_uses_steps(
        &pool,
        "pipe-r3",
        vec![step],
        dir.path(),
        &super::ResolutionPolicy::default(),
    )
    .await
    .unwrap();
    assert_eq!(out[0].uses, None);
    let reviewed: i64 =
        sqlx::query_scalar("SELECT reviewed FROM step_resolutions WHERE pipeline_id = 'pipe-r3'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reviewed, 0);
}

#[tokio::test]
async fn resolve_uses_steps_passthrough_and_provenance() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    crate::db::run_migrations(&pool, "test-secret-0123456789abcdef")
        .await
        .unwrap();

    let dir = seed_index();

    // No uses → passthrough, no rows.
    let plain = vec![plain_step()];
    let out = resolve_uses_steps(
        &pool,
        "pipe-1",
        plain.clone(),
        dir.path(),
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(out.len(), 1);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM step_resolutions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    // uses → expanded + one provenance row (webhook is required → supply it).
    let mut step = uses_step("notify/feishu@1");
    let mut with = BTreeMap::new();
    with.insert(
        "webhook".to_string(),
        "https://open.feishu/hook/x".to_string(),
    );
    step.with = Some(with);
    let out = resolve_uses_steps(&pool, "pipe-1", vec![step], dir.path(), &Default::default())
        .await
        .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].uses, None);
    assert!(out[0].run.contains("INPUT_WEBHOOK"));
    let (version, hash, source, reviewed): (String, String, String, i64) = sqlx::query_as(
        "SELECT resolved_version, content_sha256, source, reviewed FROM step_resolutions \
         WHERE pipeline_id = 'pipe-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(version, "1.4.0");
    assert_eq!(hash.len(), 64);
    assert!(source.starts_with("local:"));
    assert_eq!(reviewed, 0, "no manifest → unreviewed");
}

fn plain_step() -> StepDef {
    let mut step = uses_step("plain");
    step.uses = None;
    step.run = "echo hi".into();
    step
}
