use sha2::Digest;

use crate::services::step_resolver::{
    import_template, resolve_uses_steps, ResolutionPolicy, ResolvedStep,
};
use shared::protocol::StepDef;
use std::collections::BTreeMap;

const CURRENT: &str = env!("CARGO_PKG_VERSION");

fn feishu_spec(version: &str) -> String {
    format!(
        r#"
id: notify/feishu
name: Feishu notify
version: {version}
publisher: official
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

async fn test_pool() -> sqlx::SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    crate::db::run_migrations(&pool, "test-secret-0123456789abcdef")
        .await
        .expect("migrate db");
    pool
}

async fn seed(pool: &sqlx::SqlitePool, yaml: &str) -> (String, String, String) {
    let (outcome, id, version) = import_template(pool, yaml, "test", CURRENT)
        .await
        .expect("seed import");
    assert_eq!(
        outcome,
        crate::services::step_resolver::ImportOutcome::Inserted
    );
    let hash: String = sqlx::query_scalar(
        "SELECT content_sha256 FROM step_templates WHERE id = ? AND version = ?",
    )
    .bind(&id)
    .bind(&version)
    .fetch_one(pool)
    .await
    .expect("seeded row");
    (id, version, hash)
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

fn with_webhook(mut step: StepDef) -> StepDef {
    let mut with = BTreeMap::new();
    with.insert("webhook".to_string(), "https://hook".to_string());
    step.with = Some(with);
    step
}

#[tokio::test]
async fn import_is_idempotent_and_conflicts_on_divergent_content() {
    let pool = test_pool().await;
    let (outcome, id, version) = import_template(&pool, &feishu_spec("1.2.0"), "test", CURRENT)
        .await
        .unwrap();
    assert_eq!(
        outcome,
        crate::services::step_resolver::ImportOutcome::Inserted
    );
    assert_eq!((id.as_str(), version.as_str()), ("notify/feishu", "1.2.0"));

    // Identical content → idempotent no-op.
    let (outcome, _, _) = import_template(&pool, &feishu_spec("1.2.0"), "test", CURRENT)
        .await
        .unwrap();
    assert_eq!(
        outcome,
        crate::services::step_resolver::ImportOutcome::AlreadyPresent
    );

    // Same id+version, different content → conflict (append-only).
    let divergent = feishu_spec("1.2.0").replacen("engine: shell", "engine: ssh", 1);
    let err = import_template(&pool, &divergent, "test", CURRENT)
        .await
        .unwrap_err();
    assert!(err.contains("bump the version"), "{err}");
}

#[tokio::test]
async fn db_resolution_major_pin_exact_pin_and_sha256() {
    let pool = test_pool().await;
    seed(&pool, &feishu_spec("1.2.0")).await;
    let (_, _, hash_14) = seed(&pool, &feishu_spec("1.4.0")).await;

    let resolved = super::resolve_in_db(&pool, "notify/feishu@1", CURRENT)
        .await
        .unwrap();
    assert_eq!(resolved.spec.version, "1.4.0");

    let resolved = super::resolve_in_db(&pool, "notify/feishu@1.2.0", CURRENT)
        .await
        .unwrap();
    assert_eq!(resolved.spec.version, "1.2.0");
    assert!(resolved.source.starts_with("test"));

    let resolved = super::resolve_in_db(&pool, &format!("notify/feishu@sha256:{hash_14}"), CURRENT)
        .await
        .unwrap();
    assert_eq!(resolved.spec.version, "1.4.0");

    let err = super::resolve_in_db(&pool, "notify/absent@1", CURRENT)
        .await
        .unwrap_err();
    assert!(err.contains("not found in the step library"), "{err}");
}

#[tokio::test]
async fn stale_requires_buz_rejects_with_plugin_too_new() {
    let pool = test_pool().await;
    let future = feishu_spec("9.0.0").replacen(
        "engine: shell",
        r#"requires_buz: ">=99.0.0"
engine: shell"#,
        1,
    );

    // Ingestion gate: import refuses a template requiring a newer Buz.
    let err = import_template(&pool, &future, "test", CURRENT)
        .await
        .unwrap_err();
    assert!(err.contains("too new"), "{err}");

    // Resolution gate: a legacy row (imported before a downgrade, or via an
    // older build) is still rejected at resolution time.
    let legacy_hash = format!("{:x}", sha2::Sha256::digest(future.as_bytes()));
    sqlx::query(
        "INSERT INTO step_templates (id, version, raw_yaml, content_sha256, source)
         VALUES ('notify/feishu', '9.0.0', ?, ?, 'legacy')",
    )
    .bind(&future)
    .bind(&legacy_hash)
    .execute(&pool)
    .await
    .unwrap();
    let err = super::resolve_in_db(&pool, "notify/feishu@9", CURRENT)
        .await
        .unwrap_err();
    assert!(err.contains("too new"), "{err}");
}

#[tokio::test]
async fn expand_maps_with_to_input_env_and_clears_uses() {
    let pool = test_pool().await;
    seed(&pool, &feishu_spec("1.2.0")).await;
    let resolved: ResolvedStep = super::resolve_in_db(&pool, "notify/feishu@1.2.0", CURRENT)
        .await
        .unwrap();

    let step = with_webhook(uses_step("notify/feishu@1.2.0"));
    let expanded = super::expand_uses_step(&step, &resolved).unwrap();
    assert_eq!(expanded.engine.as_deref(), Some("shell"));
    assert!(expanded.run.contains("INPUT_WEBHOOK"));
    assert_eq!(expanded.uses, None);
    assert_eq!(expanded.with, None);
    let env = expanded.env.unwrap();
    assert_eq!(
        env.get("INPUT_WEBHOOK").map(String::as_str),
        Some("https://hook")
    );
    assert_eq!(expanded.timeout.as_deref(), Some("30s"));
}

#[tokio::test]
async fn expand_rejects_unknown_and_missing_required_inputs() {
    let pool = test_pool().await;
    seed(&pool, &feishu_spec("1.2.0")).await;
    let resolved: ResolvedStep = super::resolve_in_db(&pool, "notify/feishu@1.2.0", CURRENT)
        .await
        .unwrap();

    let mut step = uses_step("notify/feishu@1.2.0");
    let mut with = BTreeMap::new();
    with.insert("nope".to_string(), "x".to_string());
    step.with = Some(with);
    let errors = super::expand_uses_step(&step, &resolved).unwrap_err();
    assert!(
        errors.iter().any(|e| e.contains("does not declare")),
        "{errors:?}"
    );

    let step = uses_step("notify/feishu@1.2.0");
    let errors = super::expand_uses_step(&step, &resolved).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("missing required input 'webhook'")),
        "{errors:?}"
    );
}

#[tokio::test]
async fn resolve_uses_steps_passthrough_and_provenance() {
    let pool = test_pool().await;
    seed(&pool, &feishu_spec("1.2.0")).await;

    // No uses → passthrough, no rows.
    let mut plain = uses_step("plain");
    plain.uses = None;
    plain.run = "echo hi".into();
    let out = resolve_uses_steps(&pool, "pipe-1", vec![plain], &ResolutionPolicy::default())
        .await
        .unwrap();
    assert_eq!(out.len(), 1);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM step_resolutions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    // uses → expanded + one provenance row (webhook required → supply it).
    let step = with_webhook(uses_step("notify/feishu@1"));
    let out = resolve_uses_steps(&pool, "pipe-1", vec![step], &ResolutionPolicy::default())
        .await
        .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].uses, None);
    assert!(out[0].run.contains("INPUT_WEBHOOK"));
    let (version, hash, _source, reviewed): (String, String, String, i64) = sqlx::query_as(
        "SELECT resolved_version, content_sha256, source, reviewed FROM step_resolutions \
         WHERE pipeline_id = 'pipe-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(version, "1.2.0");
    assert_eq!(hash.len(), 64);
    assert_eq!(reviewed, 0, "no manifest → unreviewed");
}

#[tokio::test]
async fn reviewed_manifest_flags_hash_and_strict_mode_rejects_unreviewed() {
    let pool = test_pool().await;
    seed(&pool, &feishu_spec("1.2.0")).await;
    let resolved: ResolvedStep = super::resolve_in_db(&pool, "notify/feishu@1.2.0", CURRENT)
        .await
        .unwrap();

    let manifest_path =
        std::env::temp_dir().join(format!("reviews-{}.json", uuid::Uuid::new_v4().simple()));
    std::fs::write(
        &manifest_path,
        format!(
            r#"{{"reviews":[{{"id":"notify/feishu","version":"1.2.0","sha256":"{}"}}]}}"#,
            resolved.content_sha256
        ),
    )
    .unwrap();

    let policy = ResolutionPolicy {
        reviewed_hashes: crate::services::step_resolver::load_reviewed_hashes(
            manifest_path.to_str().unwrap(),
        ),
        require_reviewed: true,
    };

    // Reviewed hash → resolves under strict mode, provenance flagged.
    let step = with_webhook(uses_step("notify/feishu@1.2.0"));
    let out = resolve_uses_steps(&pool, "pipe-r", vec![step], &policy)
        .await
        .unwrap();
    assert_eq!(out.len(), 1);
    let reviewed: i64 =
        sqlx::query_scalar("SELECT reviewed FROM step_resolutions WHERE pipeline_id = 'pipe-r'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reviewed, 1);

    // Different (unreviewed) version → strict mode rejects.
    seed(&pool, &feishu_spec("1.3.0")).await;
    let err = resolve_uses_steps(
        &pool,
        "pipe-r2",
        vec![uses_step("notify/feishu@1.3.0")],
        &policy,
    )
    .await
    .unwrap_err();
    assert!(err.contains("not officially reviewed"), "{err}");

    // Lenient policy allows it (warning only) and records reviewed=0.
    let step = with_webhook(uses_step("notify/feishu@1.3.0"));
    let out = resolve_uses_steps(&pool, "pipe-r3", vec![step], &ResolutionPolicy::default())
        .await
        .unwrap();
    assert_eq!(out[0].uses, None);
    let reviewed: i64 =
        sqlx::query_scalar("SELECT reviewed FROM step_resolutions WHERE pipeline_id = 'pipe-r3'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reviewed, 0);

    let _ = std::fs::remove_file(&manifest_path);
}
