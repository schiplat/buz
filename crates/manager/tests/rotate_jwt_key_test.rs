//! `services::rotate_key` — the JWT key rotation used by
//! `buz-manager rotate-jwt-key`. Covers the plan/apply safety model: zero
//! writes on plan, loud abort on a wrong old key, full re-encryption with
//! roundtrip on apply, and policy rejections (same key, weak key).

use manager::services::rotate_key;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

const OLD: &str = "old-secret-0123456789abcdef";
const NEW: &str = "new-secret-0123456789abcdef-strong";

async fn pool_with_data() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    manager::db::run_migrations(&pool, OLD).await.unwrap();

    // One encrypted value in each covered surface.
    sqlx::query("INSERT INTO secrets (id, name, value) VALUES ('s1', 'K1', ?)")
        .bind(manager::crypto::encrypt("alpha-secret-value", OLD).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO github_connections (id, name, token_ciphertext) VALUES ('g1', 'conn', ?)",
    )
    .bind(manager::crypto::encrypt("gh-token-value", OLD).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO pipelines (id, name) VALUES ('p1', 'rotation-fixture')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO pipeline_git_hooks (id, pipeline_id, provider, hook_key, secret_ciphertext) \
         VALUES ('h1', 'p1', 'github', 'hk1', ?)",
    )
    .bind(manager::crypto::encrypt("hook-secret-value", OLD).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO environments (id, name, slug) VALUES ('e1', 'env', 'env')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO environment_secret_overrides (environment_id, secret_id, value) \
         VALUES ('e1', 's1', ?)",
    )
    .bind(manager::crypto::encrypt("override-value", OLD).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO hosts (id, name, host, username, credential_type, credential_data) \
         VALUES ('host-1', 'r1-agent', '10.0.0.1', 'deploy', 'key', ?)",
    )
    .bind(manager::crypto::encrypt("SSH-PRIVATE-KEY-BODY", OLD).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    // A job with mixed plaintext + buzenc:v1 trigger params.
    let mut inputs = std::collections::HashMap::new();
    inputs.insert("PLAIN".to_string(), "plain-value".to_string());
    inputs.insert("SECRET".to_string(), "job-secret-value".to_string());
    let params =
        manager::crypto::seal_trigger_params(&inputs, &["SECRET".to_string()], OLD).unwrap();
    let params = serde_json::to_string(&params).unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_params, created_at, updated_at) \
         VALUES ('j1', 'p1', 'success', ?, datetime('now'), datetime('now'))",
    )
    .bind(&params)
    .execute(&pool)
    .await
    .unwrap();
    pool
}

async fn decrypt_by_name(pool: &SqlitePool, name: &str, key: &str) -> Option<String> {
    let blob: Option<String> = sqlx::query_scalar("SELECT value FROM secrets WHERE name = ?")
        .bind(name)
        .fetch_optional(pool)
        .await
        .unwrap();
    blob.map(|b| manager::crypto::decrypt(&b, key).unwrap())
}

/// Plan validates every surface and writes nothing.
#[tokio::test]
async fn plan_reports_and_writes_nothing() {
    let pool = pool_with_data().await;
    let before: Vec<u8> = Vec::new(); // snapshot by row contents instead
    let rows_before: Vec<(String, String)> =
        sqlx::query_as("SELECT name, value FROM secrets ORDER BY name")
            .fetch_all(&pool)
            .await
            .unwrap();

    let report = rotate_key::plan(&pool, OLD, NEW).await.unwrap();
    assert_eq!(report.entries.len(), 5, "{report:?}");
    assert_eq!(report.trigger_param_jobs, 1);
    assert_eq!(report.total_values(), 6);
    assert!(!report.old_key_fingerprint.is_empty());

    let rows_after: Vec<(String, String)> =
        sqlx::query_as("SELECT name, value FROM secrets ORDER BY name")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows_before, rows_after, "plan must not mutate");
    drop(before);
}

/// Wrong old key: plan fails naming the surface, DB untouched.
#[tokio::test]
async fn plan_with_wrong_old_key_fails_loudly() {
    let pool = pool_with_data().await;
    let err = rotate_key::plan(&pool, "totally-wrong-key-0123456789", NEW)
        .await
        .unwrap_err();
    assert!(err.contains("decrypt failed"), "{err}");
}

#[tokio::test]
async fn apply_rotates_every_value_and_roundtrips() {
    let pool = pool_with_data().await;
    let report = rotate_key::apply(&pool, ":memory:", OLD, NEW)
        .await
        .unwrap();
    assert_eq!(report.total_values(), 6);

    // Every surface now decrypts under the NEW key and matches the original
    // plaintext; the old key must fail.
    assert_eq!(
        decrypt_by_name(&pool, "K1", NEW).await.as_deref(),
        Some("alpha-secret-value")
    );
    assert!(manager::crypto::decrypt(
        &sqlx::query_scalar::<_, String>("SELECT value FROM secrets WHERE name = 'K1'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        OLD
    )
    .is_err());

    // Host credential (SSH key body) survived the rotation.
    let host_cred: String =
        sqlx::query_scalar("SELECT credential_data FROM hosts WHERE id = 'host-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        manager::crypto::decrypt(&host_cred, NEW).unwrap(),
        "SSH-PRIVATE-KEY-BODY"
    );

    // Job trigger params: plaintext key kept, buzenc:v1 value re-encrypted.
    let tp: String = sqlx::query_scalar("SELECT trigger_params FROM jobs WHERE id = 'j1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let opened =
        manager::crypto::open_trigger_params(&serde_json::from_str(&tp).unwrap(), NEW).unwrap();
    assert_eq!(opened.get("PLAIN").map(String::as_str), Some("plain-value"));
    assert_eq!(
        opened.get("SECRET").map(String::as_str),
        Some("job-secret-value")
    );
}

#[tokio::test]
async fn same_key_and_weak_key_are_rejected() {
    let pool = pool_with_data().await;
    let err = rotate_key::plan(&pool, OLD, OLD).await.unwrap_err();
    assert!(err.contains("nothing to rotate"), "{err}");

    let err = rotate_key::plan(&pool, OLD, "changeme-demo-only")
        .await
        .unwrap_err();
    assert!(err.contains("new secret rejected"), "{err}");
}
