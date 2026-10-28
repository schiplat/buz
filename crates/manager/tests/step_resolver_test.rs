use manager::services::plugin_resolver::expand_uses_step;
use manager::services::plugin_resolver::load_reviewed_hashes;
use manager::services::plugin_resolver::local_plugin_dir;
use manager::services::plugin_resolver::resolve_uses_steps;
use manager::services::plugin_resolver::LocalPluginIndex;
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
source_repo: https://github.com/schiplat/buz-plugins

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

    let index = LocalPluginIndex::new(dir.path().to_path_buf());
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
    let index = LocalPluginIndex::new(dir.path().to_path_buf());

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

fn dual_engine_spec(id: &str, version: &str) -> String {
    format!(
        r#"
id: {id}
publisher: official
version: {version}
engine: shell
engines: [shell, ssh]
default_timeout: 30s
author: Buz
source_repo: https://github.com/schiplat/buz-plugins

inputs:
  url:
    type: string
    required: true
    masking: true
run: |
  curl -fsSL "$INPUT_URL"
"#
    )
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
        capabilities: Vec::new(),
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
    let dir = local_plugin_dir("/opt/buz/pipelines", "").unwrap();
    assert_eq!(dir, Path::new("/opt/buz/pipelines/plugins"));
    // Absolute dirs remain an explicit admin choice.
    let configured = local_plugin_dir("/opt/buz/pipelines", "/etc/buz/plugins").unwrap();
    assert_eq!(configured, Path::new("/etc/buz/plugins"));
    // Relative dirs anchor to the scan root and cannot escape it.
    let anchored = local_plugin_dir("/opt/buz/pipelines", "my-plugins").unwrap();
    assert_eq!(anchored, Path::new("/opt/buz/pipelines").join("my-plugins"));
    let escaped = local_plugin_dir("/opt/buz/pipelines", "../escape").unwrap_err();
    assert!(escaped.to_string().contains("escape"), "{escaped}");
}

#[test]
fn resolve_major_pin_picks_highest_version() {
    let dir = seed_index();
    let index = LocalPluginIndex::new(dir.path().to_path_buf());
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
    let index = LocalPluginIndex::new(dir.path().to_path_buf());
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
    let index = LocalPluginIndex::new(dir.path().to_path_buf());
    let err = index.resolve("buz/absent@1", "1.5.6").unwrap_err();
    assert!(err.contains("not found in the local plugin index"), "{err}");

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
    let index = LocalPluginIndex::new(dir.path().to_path_buf());
    let err = index.resolve("buz/feishu@9", "1.5.6").unwrap_err();
    assert!(err.contains("too new"), "{err}");
}

#[test]
fn expand_maps_with_to_input_env_and_clears_uses() {
    let dir = seed_index();
    let index = LocalPluginIndex::new(dir.path().to_path_buf());
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
source_repo: https://github.com/schiplat/buz-plugins
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
    let resolved = LocalPluginIndex::new(dir.path().to_path_buf())
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
    let index = LocalPluginIndex::new(dir.path().to_path_buf());
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
    let index = LocalPluginIndex::new(dir.path().to_path_buf());
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
    let strict_policy = manager::services::plugin_resolver::ResolutionPolicy {
        reviewed_hashes: manager::services::plugin_resolver::load_reviewed_hashes(
            manifest_path.to_str().unwrap(),
        ),
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
        &manager::services::plugin_resolver::ResolutionPolicy::default(),
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
    manager::db::run_migrations(&pool, "test-secret-0123456789abcdef")
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
    manager::services::plugin_resolver::replace_step_resolutions(
        &mut tx,
        "pipe-1",
        &resolved.resolutions,
    )
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

#[test]
fn resolve_rejects_capabilities_on_raw_api_steps() {
    let dir = seed_index();
    let mut step = plain_step();
    step.capabilities.push("docker-socket".into());

    let error = resolve_uses_steps(vec![step], dir.path(), &Default::default()).unwrap_err();
    assert!(
        error.contains("raw pipeline steps cannot request execution capabilities"),
        "{error}"
    );
}

fn spec_with_run_and_dir(run: &str, default_dir: Option<&str>) -> String {
    let dir_line = default_dir
        .map(|d| format!("default_dir: \"{d}\"\n"))
        .unwrap_or_default();
    format!(
        r#"
id: buz/harness
publisher: official
version: 1.0.0
engine: shell
default_timeout: 30s
{dir_line}author: Buz
source_repo: https://github.com/schiplat/buz-plugins

run: |
{run}
"#
    )
}

#[test]
fn caller_run_is_appended_after_template_run() {
    let dir = tempfile::tempdir().unwrap();
    write_spec(
        dir.path(),
        "harness.yaml",
        &spec_with_run_and_dir("  set -eu", None),
    );
    let resolved = LocalPluginIndex::new(dir.path().to_path_buf())
        .resolve("buz/harness@1.0.0", "1.8.0")
        .unwrap();

    let mut step = uses_step("buz/harness@1.0.0");
    step.run = "cargo test".into();
    let expanded = expand_uses_step(&step, &resolved).unwrap();
    assert!(expanded.run.contains("set -eu"), "{:?}", expanded.run);
    assert!(expanded.run.ends_with("cargo test"), "{:?}", expanded.run);

    // No caller run → template run verbatim.
    let plain = uses_step("buz/harness@1.0.0");
    let expanded = expand_uses_step(&plain, &resolved).unwrap();
    assert_eq!(expanded.run.trim(), "set -eu");
}

#[test]
fn default_dir_fills_step_work_dir_when_omitted() {
    let dir = tempfile::tempdir().unwrap();
    write_spec(
        dir.path(),
        "harness.yaml",
        &spec_with_run_and_dir("  set -eu", Some("source")),
    );
    let resolved = LocalPluginIndex::new(dir.path().to_path_buf())
        .resolve("buz/harness@1.0.0", "1.8.0")
        .unwrap();

    let step = uses_step("buz/harness@1.0.0");
    let expanded = expand_uses_step(&step, &resolved).unwrap();
    assert_eq!(expanded.dir.as_deref(), Some("source"));

    // Call-site work_dir wins over the template default.
    let mut step = uses_step("buz/harness@1.0.0");
    step.dir = Some("backend".into());
    let expanded = expand_uses_step(&step, &resolved).unwrap();
    assert_eq!(expanded.dir.as_deref(), Some("backend"));
}

#[test]
fn with_values_may_reference_dynamic_env() {
    let dir = tempfile::tempdir().unwrap();
    write_spec(
        dir.path(),
        "harness.yaml",
        &spec_with_run_and_dir("  set -eu", None).replacen(
            "run: |",
            "inputs:\n  tag:\n    type: string\nrun: |",
            1,
        ),
    );
    let resolved = LocalPluginIndex::new(dir.path().to_path_buf())
        .resolve("buz/harness@1.0.0", "1.8.0")
        .unwrap();

    let mut step = uses_step("buz/harness@1.0.0");
    let mut with = BTreeMap::new();
    with.insert("tag".to_string(), "v${env.APP_VERSION}".to_string());
    step.with = Some(with);
    let expanded = expand_uses_step(&step, &resolved).unwrap();
    let env = expanded.env.unwrap();
    assert_eq!(
        env.get("INPUT_TAG").map(String::as_str),
        Some("v${env.APP_VERSION}"),
        "env refs must pass through unresolved for the Runner"
    );
}

fn plain_step() -> StepDef {
    let mut step = uses_step("plain");
    step.uses = None;
    step.run = "echo hi".into();
    step
}

#[test]
fn expand_honors_call_site_engine_override_within_declared_engines() {
    let dir = tempfile::tempdir().unwrap();
    write_spec(
        dir.path(),
        "fetch-1.0.0.yaml",
        &dual_engine_spec("buz/fetch", "1.0.0"),
    );
    let index = LocalPluginIndex::new(dir.path().to_path_buf());
    let resolved = index.resolve("buz/fetch@1.0.0", "1.5.6").unwrap();

    let mut step = uses_step("buz/fetch@1.0.0");
    step.engine = Some("ssh".into());
    let mut with = BTreeMap::new();
    with.insert(
        "url".to_string(),
        "${secrets.CONFIG_BUNDLE_URL}".to_string(),
    );
    step.with = Some(with);

    let expanded = expand_uses_step(&step, &resolved).unwrap();
    assert_eq!(expanded.engine.as_deref(), Some("ssh"));
    assert!(
        expanded.host.is_none(),
        "call-site host passes through untouched"
    );
}

#[test]
fn expand_rejects_engine_override_outside_declared_engines() {
    let dir = tempfile::tempdir().unwrap();
    write_spec(
        dir.path(),
        "fetch-1.0.0.yaml",
        &dual_engine_spec("buz/fetch", "1.0.0"),
    );
    let index = LocalPluginIndex::new(dir.path().to_path_buf());
    let resolved = index.resolve("buz/fetch@1.0.0", "1.5.6").unwrap();

    let mut step = uses_step("buz/fetch@1.0.0");
    step.engine = Some("docker".into());
    let err = expand_uses_step(&step, &resolved).unwrap_err();
    assert!(
        err.iter().any(|e| e.contains("is not supported by")),
        "{err:?}"
    );
}

#[test]
fn expand_without_call_site_engine_uses_template_default() {
    let dir = tempfile::tempdir().unwrap();
    write_spec(
        dir.path(),
        "fetch-1.0.0.yaml",
        &dual_engine_spec("buz/fetch", "1.0.0"),
    );
    let index = LocalPluginIndex::new(dir.path().to_path_buf());
    let resolved = index.resolve("buz/fetch@1.0.0", "1.5.6").unwrap();

    let mut step = uses_step("buz/fetch@1.0.0");
    let mut with = BTreeMap::new();
    with.insert(
        "url".to_string(),
        "${secrets.CONFIG_BUNDLE_URL}".to_string(),
    );
    step.with = Some(with);

    let expanded = expand_uses_step(&step, &resolved).unwrap();
    assert_eq!(expanded.engine.as_deref(), Some("shell"));
}

fn feishu_call() -> StepDef {
    let mut step = uses_step("buz/feishu@1.2.0");
    let mut with = BTreeMap::new();
    with.insert("webhook".to_string(), "${secrets.FEISHU}".to_string());
    step.with = Some(with);
    step
}

#[test]
fn index_cache_picks_up_changed_template_files() {
    let dir = seed_index();

    let resolved_v1 =
        resolve_uses_steps(vec![feishu_call()], dir.path(), &Default::default()).unwrap();
    let sha_v1 = resolved_v1.resolutions[0].content_sha256.clone();
    assert!(!sha_v1.is_empty());

    // Rewrite the template with different content (different length, plus a
    // sleep so (mtime, len) differs even on coarse-grained filesystems).
    std::thread::sleep(std::time::Duration::from_millis(20));
    // Append an indented line so the run block itself grows (a top-level
    // comment would only change the raw bytes, not the parsed template).
    let mut spec_v2 = feishu_spec("buz/feishu", "1.2.0");
    spec_v2.push_str("  echo cache-invalidated\n");
    write_spec(dir.path(), "feishu-1.2.0.yaml", &spec_v2);

    let resolved_v2 =
        resolve_uses_steps(vec![feishu_call()], dir.path(), &Default::default()).unwrap();
    let sha_v2 = resolved_v2.resolutions[0].content_sha256.clone();
    assert_ne!(
        sha_v1, sha_v2,
        "rewritten template must not serve stale cache"
    );
    assert!(resolved_v2.steps[0].run.contains("cache-invalidated"));
}

#[test]
fn reviewed_manifest_cache_reloads_when_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("reviews.json");

    std::fs::write(&manifest, r#"{"reviews": [{"sha256": "aaaa"}]}"#).unwrap();
    let first = load_reviewed_hashes(manifest.to_str().unwrap());
    assert!(first.contains("aaaa"), "initial manifest loads");

    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&manifest, r#"{"reviews": [{"sha256": "bbbbbbbb"}]}"#).unwrap();
    let second = load_reviewed_hashes(manifest.to_str().unwrap());
    assert!(second.contains("bbbbbbbb"), "rewritten manifest reloads");
    assert!(!second.contains("aaaa"), "stale hashes are dropped");
}
