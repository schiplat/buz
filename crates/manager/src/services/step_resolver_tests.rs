use super::{expand_uses_step, local_step_index_dir, resolve_uses_steps, LocalStepIndex};
use shared::protocol::StepDef;
use std::collections::BTreeMap;
use std::path::Path;

const CURRENT: &str = env!("CARGO_PKG_VERSION");

fn write_spec(dir: &Path, file: &str, yaml: &str) {
    std::fs::write(dir.join(file), yaml).unwrap();
}

fn feishu_spec(id: &str, version: &str) -> String {
    format!(
        r#"
id: {id}
publisher: official
version: {version}
engine: shell
default_timeout: 30s
author: Buz
source_repo: https://github.com/buz/buz-plugins

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
        &feishu_spec("buz/feishu", "1.2.0"),
    );
    write_spec(
        dir.path(),
        "feishu-1.4.0.yaml",
        &feishu_spec("buz/feishu", "1.4.0"),
    );
    write_spec(dir.path(), "broken.yaml", "id: [broken");
    dir
}

#[test]
fn local_index_recursively_finds_specs_and_skips_hidden_or_symlink_paths() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("community/nested");
    std::fs::create_dir_all(&nested).unwrap();
    write_spec(&nested, "notify.yaml", &feishu_spec("buz/feishu", "1.2.0"));
    let hidden = dir.path().join(".hidden");
    std::fs::create_dir_all(&hidden).unwrap();
    write_spec(&hidden, "notify.yaml", &feishu_spec("buz/hidden", "1.2.0"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let external = tempfile::tempdir().unwrap();
        write_spec(
            external.path(),
            "linked.yaml",
            &feishu_spec("buz/linked", "1.2.0"),
        );
        symlink(
            external.path().join("linked.yaml"),
            dir.path().join("linked.yaml"),
        )
        .unwrap();
        symlink(external.path(), dir.path().join("linked-dir")).unwrap();
    }

    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let resolved = index.resolve("buz/feishu@1.2.0", CURRENT).unwrap();
    assert_eq!(resolved.spec.id, "buz/feishu");
    assert!(index.resolve("buz/hidden@1.2.0", CURRENT).is_err());
    assert!(index.resolve("buz/linked@1.2.0", CURRENT).is_err());
}

#[test]
fn duplicate_id_version_across_files_is_ambiguous_even_for_major_pins() {
    let dir = seed_index();
    // Two files, same id+version, different content.
    write_spec(
        dir.path(),
        "feishu-1.2.0-dup.yaml",
        &feishu_spec("buz/feishu", "1.2.0").replacen("engine: shell", "engine: ssh", 1),
    );
    let index = LocalStepIndex::new(dir.path().to_path_buf());

    // Exact pin → ambiguous.
    let err = index.resolve("buz/feishu@1.2.0", CURRENT).unwrap_err();
    assert!(err.contains("ambiguous") && err.contains("1.2.0"), "{err}");

    // Major pin → also ambiguous (would otherwise pick a file arbitrarily).
    let err = index.resolve("buz/feishu@1.2", CURRENT).unwrap_err();
    assert!(err.contains("ambiguous") && err.contains("1.2.0"), "{err}");

    // Distinct versions across files stay legitimate (@1 → highest = 1.4.0
    // from the seeded index, the 1.3.0 file doesn't disturb it).
    write_spec(
        dir.path(),
        "feishu-1.3.0.yaml",
        &feishu_spec("buz/feishu", "1.3.0"),
    );
    let resolved = index.resolve("buz/feishu@1", CURRENT).unwrap();
    assert_eq!(resolved.spec.version, "1.4.0");
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
    let resolved = index.resolve("buz/feishu@1", "1.5.6").unwrap();
    assert_eq!(resolved.spec.version, "1.4.0");

    let resolved = index.resolve("buz/feishu@1.2.0", "1.5.6").unwrap();
    assert_eq!(resolved.spec.version, "1.2.0");
    assert!(resolved.source.starts_with("local:"));
    assert_eq!(resolved.content_sha256.len(), 64);
}

#[test]
fn resolve_by_content_sha256() {
    let dir = seed_index();
    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let any = index.resolve("buz/feishu@1.2.0", "1.5.6").unwrap();
    let by_hash = index
        .resolve(
            &format!("buz/feishu@sha256:{}", any.content_sha256),
            "1.5.6",
        )
        .unwrap();
    assert_eq!(by_hash.spec.version, "1.2.0");

    let wrong = format!("buz/feishu@sha256:{}", "0".repeat(64));
    assert!(index.resolve(&wrong, "1.5.6").is_err());
}

#[test]
fn resolve_reports_missing_and_stale_requires_buz() {
    let dir = seed_index();
    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let err = index.resolve("buz/absent@1", "1.5.6").unwrap_err();
    assert!(err.contains("not found in local step index"), "{err}");

    let dir = tempfile::tempdir().unwrap();
    write_spec(
        dir.path(),
        "future.yaml",
        &feishu_spec("buz/feishu", "9.0.0").replacen(
            "engine: shell",
            "requires_buz: \">=99.0.0\"\nengine: shell",
            1,
        ),
    );
    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let err = index.resolve("buz/feishu@9", "1.5.6").unwrap_err();
    assert!(err.contains("too new"), "{err}");
}

#[test]
fn expand_maps_with_to_input_env_and_clears_uses() {
    let dir = seed_index();
    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let resolved = index.resolve("buz/feishu@1.2.0", "1.5.6").unwrap();

    let mut step = uses_step("buz/feishu@1.2.0");
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

fn numeric_spec() -> String {
    r#"
id: buz/retries
publisher: official
version: 1.2.0
engine: shell
author: Buz
source_repo: https://github.com/buz/buz-plugins
inputs:
  retries:
    type: number
    required: true
run: echo "$INPUT_RETRIES"
"#
    .to_string()
}

#[test]
fn vars_input_is_deferred_to_claim_time_validation() {
    let dir = tempfile::tempdir().unwrap();
    write_spec(dir.path(), "retries.yaml", &numeric_spec());
    let resolved = LocalStepIndex::new(dir.path().to_path_buf())
        .resolve("buz/retries@1.2.0", "1.5.6")
        .unwrap();

    let mut step = uses_step("buz/retries@1.2.0");
    let mut with = BTreeMap::new();
    with.insert("retries".to_string(), "${vars.RELEASE_TAG}".to_string());
    step.with = Some(with);

    let expanded = expand_uses_step(&step, &resolved).expect("dynamic value is deferred");
    assert_eq!(
        expanded
            .env
            .unwrap()
            .get("INPUT_RETRIES")
            .map(String::as_str),
        Some("${vars.RELEASE_TAG}")
    );
}

#[test]
fn expand_rejects_unknown_and_missing_required_inputs() {
    let dir = seed_index();
    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let resolved = index.resolve("buz/feishu@1.2.0", "1.5.6").unwrap();

    let mut step = uses_step("buz/feishu@1.2.0");
    let mut with = BTreeMap::new();
    with.insert("nope".to_string(), "x".to_string());
    step.with = Some(with);
    let errors = expand_uses_step(&step, &resolved).unwrap_err();
    assert!(
        errors.iter().any(|e| e.contains("does not declare")),
        "{errors:?}"
    );

    let step = uses_step("buz/feishu@1.2.0");
    let errors = expand_uses_step(&step, &resolved).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("missing required input 'webhook'")),
        "{errors:?}"
    );
}

#[test]
fn reviewed_manifest_flags_hash_and_strict_mode_rejects_unreviewed() {
    let dir = seed_index();
    let index = LocalStepIndex::new(dir.path().to_path_buf());
    let resolved = index.resolve("buz/feishu@1", "1.5.6").unwrap();
    let manifest_path = dir.path().join("reviews.json");
    std::fs::write(
        &manifest_path,
        format!(
            r#"{{"reviews":[{{"id":"buz/feishu","version":"1.4.0","sha256":"{}"}}]}}"#,
            resolved.content_sha256
        ),
    )
    .unwrap();
    let strict_policy = super::ResolutionPolicy {
        reviewed_hashes: super::load_reviewed_hashes(manifest_path.to_str().unwrap()),
        require_reviewed: true,
    };

    let mut reviewed_step = uses_step("buz/feishu@1.4.0");
    let mut with = BTreeMap::new();
    with.insert("webhook".to_string(), "${secrets.FEISHU}".to_string());
    reviewed_step.with = Some(with);
    let resolved = resolve_uses_steps(vec![reviewed_step], dir.path(), &strict_policy).unwrap();
    assert!(resolved.resolutions[0].reviewed);

    let err = resolve_uses_steps(
        vec![uses_step("buz/feishu@1.2.0")],
        dir.path(),
        &strict_policy,
    )
    .unwrap_err();
    assert!(err.contains("not officially reviewed"), "{err}");

    let mut unreviewed_step = uses_step("buz/feishu@1.2.0");
    let mut with = BTreeMap::new();
    with.insert("webhook".to_string(), "${secrets.FEISHU}".to_string());
    unreviewed_step.with = Some(with);
    let resolved = resolve_uses_steps(
        vec![unreviewed_step],
        dir.path(),
        &super::ResolutionPolicy::default(),
    )
    .unwrap();
    assert!(!resolved.resolutions[0].reviewed);
}

#[tokio::test]
async fn resolve_uses_steps_stages_provenance_for_caller_transaction() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    crate::db::run_migrations(&pool, "test-secret-0123456789abcdef")
        .await
        .unwrap();
    let dir = seed_index();

    let plain = resolve_uses_steps(vec![plain_step()], dir.path(), &Default::default()).unwrap();
    assert_eq!(plain.steps.len(), 1);
    assert!(plain.resolutions.is_empty());

    let mut step = uses_step("buz/feishu@1");
    let mut with = BTreeMap::new();
    with.insert("webhook".to_string(), "${secrets.FEISHU}".to_string());
    step.with = Some(with);
    let resolved = resolve_uses_steps(vec![step], dir.path(), &Default::default()).unwrap();
    assert_eq!(resolved.steps.len(), 1);
    assert_eq!(resolved.steps[0].uses, None);
    assert!(resolved.steps[0].run.contains("INPUT_WEBHOOK"));

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM step_resolutions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "resolution has no side effects before persistence"
    );

    let mut tx = pool.begin().await.unwrap();
    super::replace_step_resolutions(&mut *tx, "pipe-1", &resolved.resolutions)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let (version, hash, source, reviewed, step_order, input_defs_json): (
        String,
        String,
        String,
        i64,
        i64,
        String,
    ) = sqlx::query_as(
        "SELECT resolved_version, content_sha256, source, reviewed, step_order, input_defs_json \
             FROM step_resolutions WHERE pipeline_id = 'pipe-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(version, "1.4.0");
    assert_eq!(hash.len(), 64);
    assert!(source.starts_with("local:"));
    assert_eq!(reviewed, 0);
    assert_eq!(step_order, 0);
    let input_defs: Vec<shared::protocol::PipelineInputDef> =
        serde_json::from_str(&input_defs_json).unwrap();
    assert_eq!(input_defs.len(), 1);
    assert_eq!(input_defs[0].name, "webhook");
}

fn plain_step() -> StepDef {
    let mut step = uses_step("plain");
    step.uses = None;
    step.run = "echo hi".into();
    step
}
