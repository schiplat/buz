//! Built-in sample pipeline: seeded once on fresh installs, never resurrected
//! after deletion, and available explicitly through `seed_forced`.

mod common;

use common::{test_config, test_db};
use manager::db;
use manager::services::sample_pipeline::{
    seed_forced, seed_if_fresh, SeedOutcome, META_KEY, SAMPLE_NAME, SAMPLE_YAML,
};
use manager::store::exec;

async fn fresh_pool() -> manager::store::exec::DbPool {
    let pool = test_db(4).await;
    db::seed_default_rows(&pool).await.expect("seed defaults");
    manager::rbac::bootstrap(&pool).await.expect("bootstrap");
    pool
}

async fn pipeline_count(pool: &manager::store::exec::DbPool) -> i64 {
    exec::fetch_scalar_i64(pool, "SELECT COUNT(*) FROM pipelines", &[])
        .await
        .unwrap()
}

#[tokio::test]
async fn fresh_install_gets_one_editable_sample() {
    let pool = fresh_pool().await;
    let config = test_config();

    let outcome = seed_if_fresh(&pool, &config).await.expect("seed");
    assert_eq!(outcome, SeedOutcome::Seeded);
    assert_eq!(pipeline_count(&pool).await, 1);

    let row = exec::fetch_optional(
        &pool,
        "SELECT p.id, p.name, p.team_id, p.trigger_type, p.is_active, p.input_defs, p.yaml_source
         FROM pipelines p WHERE p.name = ?",
        &[SAMPLE_NAME.into()],
    )
    .await
    .expect("query sample")
    .expect("sample row");
    assert_eq!(row.text(3), "manual");
    assert_eq!(row.i64(4), 1);
    let input_defs = row.text(5);
    assert!(
        input_defs.contains("\"name\"") && input_defs.contains("Buz"),
        "input definition missing: {input_defs}"
    );
    // YAML is the stored authority and carries the documented input reference.
    assert!(row.text(6).contains("$INPUT_NAME"));

    let team_id = row.text(2);
    let slug = exec::fetch_optional(
        &pool,
        "SELECT slug FROM teams WHERE id = ?",
        &[team_id.as_str().into()],
    )
    .await
    .expect("team slug")
    .map(|r| r.text(0));
    assert_eq!(slug.as_deref(), Some("default"));

    let tasks = exec::fetch_scalar_i64(
        &pool,
        "SELECT COUNT(*) FROM tasks WHERE pipeline_id = ?",
        &[row.text(0).into()],
    )
    .await
    .expect("task count");
    assert_eq!(tasks, 1, "sample has exactly one shell step");

    // The marker makes the seed one-shot.
    let again = seed_if_fresh(&pool, &config).await.expect("second boot");
    assert_eq!(again, SeedOutcome::AlreadyMarked);
    assert_eq!(pipeline_count(&pool).await, 1);
    assert_eq!(
        db::manager_meta_get(&pool, META_KEY)
            .await
            .unwrap()
            .as_deref(),
        Some("seeded")
    );
}

#[tokio::test]
async fn existing_install_is_skipped_and_marked() {
    let pool = fresh_pool().await;
    exec::execute(
        &pool,
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_ops, is_active)
         VALUES ('existing', 'existing', 'hash', 'existing', 0, 0, 0, 1)",
        &[],
    )
    .await
    .expect("insert user");

    let outcome = seed_if_fresh(&pool, &test_config()).await.expect("seed");
    assert_eq!(outcome, SeedOutcome::SkippedExisting);
    assert_eq!(pipeline_count(&pool).await, 0);
    assert_eq!(
        db::manager_meta_get(&pool, META_KEY)
            .await
            .unwrap()
            .as_deref(),
        Some("skipped-existing")
    );
}

#[tokio::test]
async fn deleted_sample_is_not_recreated_but_can_be_restored() {
    let pool = fresh_pool().await;
    let config = test_config();
    assert_eq!(
        seed_if_fresh(&pool, &config).await.expect("seed"),
        SeedOutcome::Seeded
    );

    exec::execute(
        &pool,
        "DELETE FROM pipelines WHERE name = ?",
        &[SAMPLE_NAME.into()],
    )
    .await
    .expect("delete sample");
    assert_eq!(pipeline_count(&pool).await, 0);

    // Startup must not resurrect a deliberately deleted sample.
    assert_eq!(
        seed_if_fresh(&pool, &config).await.expect("reboot"),
        SeedOutcome::AlreadyMarked
    );
    assert_eq!(pipeline_count(&pool).await, 0);

    // The explicit operator command may restore it.
    assert_eq!(
        seed_forced(&pool, &config).await.expect("restore"),
        SeedOutcome::Seeded
    );
    assert_eq!(pipeline_count(&pool).await, 1);
}

#[test]
fn sample_yaml_matches_the_documented_shape() {
    let parsed: shared::pipeline_yaml::PipelineYaml =
        serde_yaml::from_str(SAMPLE_YAML).expect("sample YAML parses");
    assert_eq!(parsed.name, SAMPLE_NAME);
    assert_eq!(parsed.team.as_deref(), Some("default"));
    assert_eq!(parsed.environments, vec!["default".to_string()]);
    let inputs = parsed
        .on
        .and_then(|on| on.manual)
        .and_then(|manual| manual.inputs)
        .expect("manual inputs");
    assert_eq!(inputs["name"].default.as_deref(), Some("Buz"));
    assert_eq!(parsed.steps.len(), 1);
    assert_eq!(parsed.steps[0].engine.as_deref(), Some("shell"));
}
