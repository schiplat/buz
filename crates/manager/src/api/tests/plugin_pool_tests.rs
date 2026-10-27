//! Step pool integration (step-marketplace §6.8 Phase 1): a remote source's
//! index is synced into the pool, curated on, and a pipeline step referencing
//! it resolves at save time over HTTP — with review-gate and branch-pin
//! behavior covered.

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};
use sqlx::sqlite::SqlitePoolOptions;
use tower::ServiceExt;

use crate::api;
use crate::config::{AuthConfig, Config};
use crate::db::run_migrations;
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const ADMIN_ID: &str = "admin-1";

fn community_spec() -> String {
    r#"
id: alice/oss-upload
version: 1.0.0
publisher: community
author: alice
source_repo: https://github.com/alice/my-buz-steps
engine: shell
requires_buz: ">=1.0.0"
inputs:
  bucket:
    type: string
    required: true
  file:
    type: string
    required: true
run: |
  set -eu
  : "${INPUT_BUCKET:?}" "${INPUT_FILE:?}"
  echo "upload ${INPUT_FILE} -> ${INPUT_BUCKET}"
"#
    .into()
}

fn index_json(good_sha: &str) -> String {
    let broken_hash = "0".repeat(64);
    format!(
        r#"{{ "steps": [
            {{ "id": "alice/oss-upload", "version": "1.0.0",
               "file": "steps/oss-upload/1.0.0.yaml", "sha256": "{good_sha}" }},
            {{ "id": "alice/broken-fetch", "version": "0.1.0",
               "file": "steps/broken-fetch/0.1.0.yaml", "sha256": "{}" }},
            {{ "id": "alice/broken-hash", "version": "0.1.1",
               "file": "steps/broken-hash.yaml", "sha256": "{broken_hash}" }} ] }}"#,
        "b".repeat(64)
    )
}

/// Serves the community source: index.json, the template YAML, and a
/// deliberately missing branch index (for the branch-pin error path).
async fn spawn_source_server(yaml: String, yaml_sha: String) -> String {
    let yaml = std::sync::Arc::new(yaml);
    let yaml_sha = std::sync::Arc::new(yaml_sha);
    let yaml_sha_for_meta = yaml_sha.clone();
    let yaml_sha_for_revision = yaml_sha.clone();
    let app = Router::new()
        .route(
            "/index.json",
            axum::routing::get(move || {
                let sha = yaml_sha.clone();
                async move { index_json(&sha) }
            }),
        )
        .route(
            "/steps/oss-upload/1.0.0.yaml",
            axum::routing::get(move || {
                let body = (*yaml).clone();
                async move { body }
            }),
        )
        .route(
            "/steps/broken-hash.yaml",
            axum::routing::get(|| async {
                "actual content does not match the declared zeros hash"
            }),
        )
        .route(
            "/steps/oss-upload/meta.json",
            axum::routing::get(move || {
                let sha = yaml_sha_for_meta.clone();
                async move {
                    // meta lists a NEW version the index does not have —
                    // only the meta path can discover it.
                    format!(
                        r#"{{ "id": "alice/oss-upload", "versions": [
                            {{ "version": "1.1.0", "file": "steps/oss-upload/1.1.0.yaml", "sha256": "{}" }},
                            {{ "version": "1.0.0", "file": "steps/oss-upload/1.0.0.yaml", "sha256": "{sha}" }} ] }}"#,
                        "c".repeat(64)
                    )
                }
            }),
        )
        .route(
            "/reviews.json",
            axum::routing::get(|| async { r#"{"reviews": []}"# }),
        )
        .route(
            "/revision.json",
            axum::routing::get(move || {
                let sha = yaml_sha_for_revision.clone();
                async move {
                    format!(
                        r#"{{ "revision": "{}", "updated_at": "2026-01-01T00:00:00Z" }}"#,
                        &sha[..12.min(sha.len())]
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn test_app() -> (Router, sqlx::SqlitePool) {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 1, 0, 1)",
    )
    .bind(ADMIN_ID)
    .bind("boss")
    .bind("boss")
    .execute(&pool)
    .await
    .expect("seed admin");
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')")
        .execute(&pool)
        .await
        .expect("seed team");
    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, max_concurrent_jobs)
         VALUES ('runner-1', 'r1', 'tok', '[\"linux-x64\"]', 'online', 2)",
    )
    .execute(&pool)
    .await
    .expect("seed runner");

    let state = AppState {
        pool: pool.clone(),
        config: Config {
            server: Default::default(),
            database: Default::default(),
            auth: AuthConfig {
                jwt_secret: JWT_SECRET.into(),
                jwt_ttl_hours: 24,
                runner_register_token_ttl_hours: 24,
            },
            runner: Default::default(),
            pipeline: Default::default(),
            standalone: Default::default(),
            plugin_index: Default::default(),
            storage: Default::default(),
            clock: Default::default(),
            logging: Default::default(),
            marketplace_url: String::new(),
        },
        log_broadcast: LogBroadcaster::new(),
    };
    (Router::new().nest("/api/v1", api::router(state)), pool)
}

async fn read_json(response: axum::response::Response) -> serde_json::Value {
    let body = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    serde_json::from_slice(&body)
        .unwrap_or_else(|e| panic!("json body ({}): {e}", String::from_utf8_lossy(&body)))
}

async fn add_source(app: &Router, jwt: &str, url: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/admin/plugin-sources")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "name": "alice", "url": url, "auto_enable": false })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = read_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn save_pipeline(app: &Router, jwt: &str, uses: &str) -> (StatusCode, serde_json::Value) {
    let payload = serde_json::json!({
        "name": "pool-smoke",
        "trigger_type": "manual",
        "environments": ["default"],
        "runner_labels": ["linux-x64"],
        "team_id": "team-1",
        "steps": [
            { "name": "upload", "uses": uses,
              "with": { "bucket": "s3://x", "file": "a.txt" } }
        ]
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = read_json(resp).await;
    (status, body)
}

#[tokio::test]
async fn community_source_curates_resolves_and_gates() {
    let yaml = community_spec();
    let yaml_sha = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(yaml.as_bytes()))
    };
    let source_url = spawn_source_server(yaml.clone(), yaml_sha.clone()).await;
    let (app, pool) = test_app().await;
    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap();

    // 1. Add the source: the API stages the row and syncs in the background.
    // Poll until the background fetch lands (staging the pool rows).
    let body = add_source(&app, &jwt, &format!("{source_url}/index.json")).await;
    assert_eq!(body["data"]["syncing"], true, "{body}");
    assert_eq!(body["data"]["source"]["auto_enable"], false);
    let source_id = body["data"]["source"]["id"]
        .as_str()
        .expect("source id")
        .to_string();
    // The spawned background sync races the mock server in this
    // single-threaded test runtime, so the fetch is exercised
    // deterministically here (same call the spawn performs).
    let source = crate::services::plugin_pool::get_source(&pool, &source_id)
        .await
        .unwrap()
        .expect("source row");
    let http = crate::services::plugin_pool::http_client(60);
    let count =
        crate::services::plugin_pool::sync_source(&http, &pool, &source, shared::version::VERSION)
            .await
            .expect("deterministic sync of the staged source");
    assert_eq!(count, 3, "index entries staged");

    // 2. Pool rows exist but are staged (disabled) — resolution must fail.
    let (status, body) = save_pipeline(&app, &jwt, "alice/oss-upload@1.0.0").await;
    assert_ne!(
        status,
        StatusCode::OK,
        "staged template must not resolve: {body}"
    );

    // 3a. Enabling a broken entry is REJECTED (file 404) and stays staged.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/admin/plugin-pool/alice/broken-fetch/0.1.0")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{ "enabled": true }"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let staged: (i64,) =
        sqlx::query_as("SELECT enabled FROM plugin_pool WHERE step_id = 'broken-fetch'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(staged.0, 0, "failed enable must keep the entry staged");

    // 3b. Enabling an entry whose content hash mismatches the index → rejected.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/admin/plugin-pool/alice/broken-hash/0.1.1")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{ "enabled": true }"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // 3c. Curator enables the good entry.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/admin/plugin-pool/alice/oss-upload/1.0.0")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{ "enabled": true }"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 1b. Per-plugin refresh via meta.json: discovers 1.1.0 (staged, since
    // the meta path never auto-enables) and preserves the enabled 1.0.0.
    let refreshed = crate::services::plugin_pool::refresh_plugin(
        &reqwest::Client::new(),
        &pool,
        "alice",
        "oss-upload",
        shared::version::VERSION,
    )
    .await
    .expect("meta refresh");
    assert_eq!(
        refreshed.len(),
        2,
        "meta discovered the new version: {refreshed:?}"
    );
    let by_version = |v: &str| {
        refreshed
            .iter()
            .find(|r| r.version == v)
            .unwrap_or_else(|| panic!("version {v} missing"))
            .clone()
    };
    eprintln!("DEBUG refreshed rows: {refreshed:?}");
    assert!(
        by_version("1.0.0").enabled,
        "curator verdict survives; rows: {refreshed:?}"
    );
    assert!(
        !by_version("1.1.0").enabled,
        "new meta version lands staged"
    );

    // 4. Save-time resolution now succeeds over HTTP and records provenance.
    let (status, body) = save_pipeline(&app, &jwt, "alice/oss-upload@1.0.0").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pipeline_id = body["data"]["id"]
        .as_str()
        .expect("pipeline id")
        .to_string();
    let resolutions: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM step_resolutions WHERE pipeline_id = ?")
            .bind(&pipeline_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        resolutions.0, 1,
        "provenance recorded for the remote template"
    );

    // 5. sha256 pin resolves to the same content.
    let (status, body) =
        save_pipeline(&app, &jwt, &format!("alice/oss-upload@sha256:{yaml_sha}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 5b. Per-plugin refresh: the source publishes a new version; refreshing
    // the plugin stages 1.1.0 while the curator-enabled 1.0.0 keeps its state.
    // The mock index serves a second entry for the same plugin.
    // (The spawn_source_server index is fixed; extend it via a second server
    // is overkill — instead drive refresh against the same index plus a
    // hand-inserted row simulating a curator-enabled older version.)
    let refreshed = crate::services::plugin_pool::refresh_plugin(
        &reqwest::Client::new(),
        &pool,
        "alice",
        "oss-upload",
        shared::version::VERSION,
    )
    .await
    .expect("per-plugin refresh");
    // The index still lists only 1.0.0 → refresh is a no-op for versions,
    // and the enabled 1.0.0 row survives with its verdict intact.
    assert!(
        refreshed.iter().any(|r| r.version == "1.0.0" && r.enabled),
        "curator verdict must survive per-plugin refresh: {refreshed:?}"
    );

    // 6. Strict review gate (via the settings API so the runtime override
    // applies): enabling requires an existing manifest file — provision an
    // empty per-test one via the manifest override (tempdir: parallel tests
    // must not see each other's files), so the enabled-but-unreviewed save
    // is rejected.
    let manifest_dir = tempfile::tempdir().unwrap();
    let manifest_path = manifest_dir.path().join("reviews.json");
    std::fs::write(&manifest_path, r#"{"reviews": []}"#).unwrap();
    let manifest = manifest_path.to_string_lossy().into_owned();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/admin/settings/plugin_reviewed_manifest")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "value": manifest }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/admin/settings/plugin_review_required")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{ "value": "true" }"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (status, body) = save_pipeline(&app, &jwt, "alice/oss-upload@1.0.0").await;
    assert_ne!(status, StatusCode::OK, "{body}");
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("review"),
        "gate error must mention review: {message}"
    );

    // 7. Branch pins surface a clear error when the branch has no index.
    let (status, body) = save_pipeline(&app, &jwt, "alice/oss-upload@main").await;
    assert_ne!(status, StatusCode::OK, "{body}");
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("branch") || message.contains("main"),
        "branch pin error should name the ref: {message}"
    );
}

#[tokio::test]
async fn official_source_seeds_and_startup_sync_populates_pool() {
    // The migration pre-seeds the official source (auto_enable) — a fresh
    // database must already carry it.
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

    let seeded: (String, String, i64, i64) = sqlx::query_as(
        "SELECT name, url, auto_enable, enabled FROM plugin_sources WHERE id = 'official'",
    )
    .fetch_one(&pool)
    .await
    .expect("official source seeded");
    assert_eq!(seeded.0, "Official buz-plugins");
    assert!(
        seeded.2 != 0,
        "auto_enable must be on for the official source"
    );
    assert!(seeded.3 != 0, "official source must be enabled");

    // Startup sync (sync_all_enabled) pulls the index and auto-enables every
    // entry — the "official templates work out of the box" contract.
    let yaml = r#"
id: buz/demo
version: 1.0.0
publisher: official
author: buz maintainers
engine: shell
inputs:
  name:
    type: string
    required: true
run: |
  echo "hello ${INPUT_NAME}"
"#
    .replace(
        "\n", "
",
    );
    let yaml_sha = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(yaml.as_bytes()))
    };
    let app = Router::new()
        .route(
            "/index.json",
            axum::routing::get(move || {
                let body = format!(
                    r#"{{ "steps": [ {{ "id": "buz/demo", "version": "1.0.0",
                       "file": "steps/demo/1.0.0.yaml", "sha256": "{yaml_sha}" }} ] }}"#
                );
                async move { body }
            }),
        )
        .route(
            "/steps/demo/1.0.0.yaml",
            axum::routing::get(move || {
                let body = yaml.clone();
                async move { body }
            }),
        )
        .route(
            "/reviews.json",
            axum::routing::get(|| async { r#"{"reviews": [{"sha256": "PLACEHOLDER"}]}"# }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Point the official source at the mock and run the startup sync.
    sqlx::query("UPDATE plugin_sources SET url = ? WHERE id = 'official'")
        .bind(format!("http://{addr}/index.json"))
        .execute(&pool)
        .await
        .unwrap();
    let synced = crate::services::plugin_pool::sync_all_enabled(
        &pool,
        &reqwest::Client::new(),
        shared::version::VERSION,
    )
    .await;
    assert_eq!(synced, 1);

    let entry: (String, i64, i64) =
        sqlx::query_as("SELECT version, enabled, reviewed FROM plugin_pool WHERE step_id = 'demo'")
            .fetch_one(&pool)
            .await
            .expect("pool row for buz/demo");
    assert_eq!(entry.0, "1.0.0");
    assert_eq!(entry.1, 1, "official auto_enable must enable entries");
    // reviews.json placeholder hash never matches → no badge (strict mode
    // would reject; official entries carry CI-signed hashes in production).
    assert_eq!(entry.2, 0);
}

/// Find one plugin's pool row in a test assertion (panics with the id).
fn pool_row_for<'a>(
    rows: &'a [crate::services::plugin_pool::PoolRow],
    step_id: &str,
) -> &'a crate::services::plugin_pool::PoolRow {
    rows.iter()
        .find(|row| row.step_id == step_id)
        .unwrap_or_else(|| panic!("pool row for bob/{step_id}"))
}

/// Sync persists a per-entry compatibility verdict: the compatible template
/// passes, a too-new `requires_buz` is flagged with the gate's exact message,
/// a missing template stays unknown (NULL), and a manager version bump
/// re-verifies every entry on the next sync.
#[tokio::test]
async fn sync_persists_compatibility_verdicts() {
    let compat_spec = r#"
id: bob/compatible
version: 1.0.0
publisher: community
author: bob
source_repo: https://github.com/bob/steps
engine: shell
requires_buz: ">=1.0.0"
inputs:
  name:
    type: string
    required: true
run: |
  echo "hello ${INPUT_NAME}"
"#
    .to_string();
    let too_new_spec = r#"
id: bob/too-new
version: 1.0.0
publisher: community
author: bob
source_repo: https://github.com/bob/steps
engine: shell
requires_buz: ">=99.0.0"
inputs:
  name:
    type: string
    required: true
run: |
  echo "hello ${INPUT_NAME}"
"#
    .to_string();
    let missing_spec = r#"
id: bob/missing
version: 1.0.0
publisher: community
author: bob
source_repo: https://github.com/bob/steps
engine: shell
inputs:
  name:
    type: string
    required: true
run: |
  echo "hello ${INPUT_NAME}"
"#
    .to_string();
    let sha = |raw: &str| {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(raw.as_bytes()))
    };
    let (compat_sha, too_new_sha, missing_sha) =
        (sha(&compat_spec), sha(&too_new_spec), sha(&missing_spec));

    let index = format!(
        r#"{{ "steps": [
            {{ "id": "bob/compatible", "version": "1.0.0",
               "file": "steps/compatible/1.0.0.yaml", "sha256": "{compat_sha}" }},
            {{ "id": "bob/too-new", "version": "1.0.0",
               "file": "steps/too-new/1.0.0.yaml", "sha256": "{too_new_sha}" }},
            {{ "id": "bob/missing", "version": "1.0.0",
               "file": "steps/missing/1.0.0.yaml", "sha256": "{missing_sha}" }} ] }}"#
    );
    let app = Router::new()
        .route(
            "/index.json",
            axum::routing::get(move || {
                let index = index.clone();
                async move { index }
            }),
        )
        .route(
            "/steps/compatible/1.0.0.yaml",
            axum::routing::get(move || {
                let body = compat_spec.clone();
                async move { body }
            }),
        )
        .route(
            "/steps/too-new/1.0.0.yaml",
            axum::routing::get(move || {
                let body = too_new_spec.clone();
                async move { body }
            }),
        );
    // bob/missing is deliberately NOT routed — the fetch 404s.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");
    sqlx::query(
        "INSERT INTO plugin_sources (id, name, url, auto_enable, enabled) \
         VALUES ('src-compat', 'compat', ?, 0, 1)",
    )
    .bind(format!("http://{addr}/index.json"))
    .execute(&pool)
    .await
    .unwrap();
    let source = crate::services::plugin_pool::get_source(&pool, "src-compat")
        .await
        .unwrap()
        .expect("source row");
    let http = crate::services::plugin_pool::http_client(60);

    // First sync: compatible → Some(true), too-new → Some(false) with the
    // gate's message, missing template → unknown (NULL, sync still succeeds).
    let count =
        crate::services::plugin_pool::sync_source(&http, &pool, &source, shared::version::VERSION)
            .await
            .expect("sync with compatibility verdicts");
    assert_eq!(count, 3);
    let progress = crate::services::sync_progress::snapshot("src-compat").expect("progress");
    assert_eq!(progress.phase, "done");
    assert_eq!(progress.total_entries, 3);
    assert_eq!(progress.done_entries, 3);
    assert_eq!(progress.done_plugins, 3);
    assert!(progress.error.is_none());
    let rows = crate::services::plugin_pool::list_pool(&pool)
        .await
        .unwrap();
    let compatible = pool_row_for(&rows, "compatible");
    assert_eq!(compatible.buz_compatible, Some(true));
    assert_eq!(compatible.requires_buz.as_deref(), Some(">=1.0.0"));
    assert!(compatible.compat_error.is_none());
    let too_new = pool_row_for(&rows, "too-new");
    assert_eq!(too_new.buz_compatible, Some(false));
    let error = too_new.compat_error.as_deref().expect("compat error");
    assert!(error.contains("requires_buz"), "{error}");
    assert!(error.contains(">= 99.0.0"), "{error}");
    assert!(error.contains("too new"), "{error}");
    let missing = pool_row_for(&rows, "missing");
    assert_eq!(missing.buz_compatible, None, "fetch failure = unknown");
    assert!(missing.compat_error.is_none());

    // The rows the API ships must expose the verdict fields (serde names
    // the dashboard's PoolEntry contract relies on).
    let payload = serde_json::to_value(&rows).unwrap();
    for field in ["requires_buz", "buz_compatible", "compat_error"] {
        assert!(
            payload[0].get(field).is_some(),
            "pool row JSON missing {field}: {payload}"
        );
    }

    // Re-sync without a version change: cached verdicts are kept.
    crate::services::plugin_pool::sync_source(&http, &pool, &source, shared::version::VERSION)
        .await
        .expect("re-sync");
    let rows = crate::services::plugin_pool::list_pool(&pool)
        .await
        .unwrap();
    assert_eq!(pool_row_for(&rows, "too-new").buz_compatible, Some(false));

    // Manager upgrade: syncing under a newer build re-verifies — the
    // previously too-new entry becomes compatible.
    crate::services::plugin_pool::sync_source(&http, &pool, &source, "99.0.0")
        .await
        .expect("re-sync under new manager version");
    let rows = crate::services::plugin_pool::list_pool(&pool)
        .await
        .unwrap();
    let too_new = pool_row_for(&rows, "too-new");
    assert_eq!(
        too_new.buz_compatible,
        Some(true),
        "version bump must re-verify: {:?}",
        too_new.compat_error
    );
    assert_eq!(too_new.requires_buz.as_deref(), Some(">=99.0.0"));
}

/// A plugin the pool has never seen initializes with exactly ONE enabled
/// version — the latest COMPATIBLE one — on auto-enable sources. Older and
/// incompatible versions stage, and once the curator flips any switch their
/// verdict survives every later sync.
#[tokio::test]
async fn auto_enable_source_initializes_latest_compatible_version_only() {
    fn tool_spec(version: &str, requires: &str) -> String {
        format!(
            r#"
id: car/tool
version: {version}
publisher: community
author: carol
source_repo: https://github.com/carol/tools
engine: shell
requires_buz: "{requires}"
inputs:
  name:
    type: string
    required: true
run: |
  echo "hello ${{INPUT_NAME}}"
"#
        )
    }
    let versions: [(&str, &str); 4] = [
        ("2.0.0", ">=99.0.0"), // latest overall — incompatible
        ("1.1.0", ">=1.0.0"),  // latest compatible → the init enable
        ("1.0.0", ">=1.0.0"),
        ("0.9.0", ">=1.0.0"),
    ];
    let sha = |raw: &str| {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(raw.as_bytes()))
    };
    let index = format!(
        r#"{{ "steps": [ {} ] }}"#,
        versions
            .iter()
            .map(|(version, requires)| {
                format!(
                    r#"{{ "id": "car/tool", "version": "{version}",
                       "file": "steps/tool/{version}.yaml", "sha256": "{}" }}"#,
                    sha(&tool_spec(version, requires))
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut app = Router::new().route(
        "/index.json",
        axum::routing::get(move || {
            let index = index.clone();
            async move { index }
        }),
    );
    for (version, requires) in versions {
        let body = tool_spec(version, requires);
        app = app.route(
            format!("/steps/tool/{version}.yaml").as_str(),
            axum::routing::get(move || {
                let body = body.clone();
                async move { body }
            }),
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");
    sqlx::query(
        "INSERT INTO plugin_sources (id, name, url, auto_enable, enabled) \
         VALUES ('src-auto', 'auto', ?, 1, 1)",
    )
    .bind(format!("http://{addr}/index.json"))
    .execute(&pool)
    .await
    .unwrap();
    let source = crate::services::plugin_pool::get_source(&pool, "src-auto")
        .await
        .unwrap()
        .expect("source row");
    let http = crate::services::plugin_pool::http_client(60);

    crate::services::plugin_pool::sync_source(&http, &pool, &source, shared::version::VERSION)
        .await
        .expect("init sync");

    let enabled: Vec<String> =
        sqlx::query_as("SELECT version FROM plugin_pool WHERE enabled = 1 ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|(v,): (String,)| v)
            .collect();
    assert_eq!(
        enabled,
        vec!["1.1.0".to_string()],
        "exactly the latest compatible version initializes enabled"
    );
    let too_new: (i64, Option<i64>) =
        sqlx::query_as("SELECT enabled, buz_compatible FROM plugin_pool WHERE version = '2.0.0'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(too_new, (0, Some(0)), "incompatible latest stays staged");

    // The curator locks the plugin (disables 1.1.0): later syncs must keep
    // every verdict — no version quietly re-enables, not even on re-check.
    sqlx::query("UPDATE plugin_pool SET enabled = 0 WHERE version = '1.1.0'")
        .execute(&pool)
        .await
        .unwrap();
    crate::services::plugin_pool::sync_source(&http, &pool, &source, shared::version::VERSION)
        .await
        .expect("re-sync after curator lock");
    let (enabled,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM plugin_pool WHERE enabled = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(enabled, 0, "curator's disable survives the re-sync");
}

/// A full-meta source is probed on staging: served templates get
/// content-verified verdicts through the gate evaluation, and an index that
/// lists a file which is NOT published (the half-uploaded mirror case) turns
/// red at sync time — not at enable time.
#[tokio::test]
async fn full_meta_source_probes_files_on_staging() {
    let index = format!(
        r#"{{ "steps": [
            {{ "id": "dave/fast", "version": "1.0.0", "file": "steps/fast/1.0.0.yaml",
               "sha256": "{fast_sha}", "requires_buz": ">=1.0.0", "engine": "shell",
               "category": "source", "name": "Fast checkout",
               "description": "Check out a repository fast.",
               "description_zh": "快速检出仓库。",
               "author": "dave", "publisher": "community",
               "license": "Apache-2.0" }},
            {{ "id": "dave/portal", "version": "1.0.0", "file": "steps/portal/1.0.0.yaml",
               "sha256": "{portal_sha}", "requires_buz": ">=99.0.0" }},
            {{ "id": "dave/ghost", "version": "1.0.0", "file": "steps/ghost/1.0.0.yaml",
               "sha256": "{ghost_sha}", "requires_buz": ">=1.0.0" }} ] }}"#,
        fast_sha = "e".repeat(64),
        portal_sha = "f".repeat(64),
        ghost_sha = "7".repeat(64),
    );
    let sha = |raw: String| {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(raw.as_bytes()))
    };
    let fast_spec = format!(
        "id: dave/fast\nversion: 1.0.0\npublisher: community\nauthor: dave\n\
         source_repo: https://github.com/dave/steps\nengine: shell\n\
         requires_buz: \">=1.0.0\"\ninputs:\n  name:\n    type: string\n    required: true\n\
         run: |\n  echo hello\n"
    );
    let portal_spec = fast_spec
        .replace("dave/fast", "dave/portal")
        .replace(">=1.0.0", ">=99.0.0");
    let (fast_sha, portal_sha) = (sha(fast_spec.clone()), sha(portal_spec.clone()));
    let index = index
        .replace(&"e".repeat(64), &fast_sha)
        .replace(&"f".repeat(64), &portal_sha);
    let app = Router::new()
        .route(
            "/index.json",
            axum::routing::get(move || {
                let index = index.clone();
                async move { index }
            }),
        )
        .route(
            "/steps/fast/1.0.0.yaml",
            axum::routing::get(move || {
                let body = fast_spec.clone();
                async move { body }
            }),
        )
        .route(
            "/steps/portal/1.0.0.yaml",
            axum::routing::get(move || {
                let body = portal_spec.clone();
                async move { body }
            }),
        );
    // dave/ghost is listed in the index but deliberately NOT published.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");
    sqlx::query(
        "INSERT INTO plugin_sources (id, name, url, auto_enable, enabled) \
         VALUES ('src-meta', 'meta', ?, 0, 1)",
    )
    .bind(format!("http://{addr}/index.json"))
    .execute(&pool)
    .await
    .unwrap();
    let source = crate::services::plugin_pool::get_source(&pool, "src-meta")
        .await
        .unwrap()
        .expect("source row");
    let http = crate::services::plugin_pool::http_client(60);

    crate::services::plugin_pool::sync_source(&http, &pool, &source, shared::version::VERSION)
        .await
        .expect("sync from full-meta index");

    let rows = crate::services::plugin_pool::list_pool(&pool)
        .await
        .unwrap();
    let row = |step_id: &str| {
        rows.iter()
            .find(|row| row.step_id == step_id)
            .unwrap_or_else(|| panic!("pool row for dave/{step_id}"))
    };
    // Served + gate-satisfying → content-verified green.
    let fast = row("fast");
    assert_eq!(fast.buz_compatible, Some(true));
    assert_eq!(fast.requires_buz.as_deref(), Some(">=1.0.0"));
    let meta = fast.meta.as_ref().expect("full meta persisted on the row");
    assert_eq!(meta.name.as_deref(), Some("Fast checkout"));
    assert_eq!(meta.description_zh.as_deref(), Some("快速检出仓库。"));
    assert_eq!(meta.category.as_deref(), Some("source"));
    assert_eq!(meta.publisher.as_deref(), Some("community"));
    // The API payload the dashboard consumes exposes the meta object.
    let payload = serde_json::to_value(&rows).unwrap();
    assert_eq!(payload[0]["meta"]["name"], "Fast checkout");
    // Served but gate-unsatisfying → red with the gate's message.
    let portal = row("portal");
    assert_eq!(portal.buz_compatible, Some(false));
    let error = portal
        .compat_error
        .as_deref()
        .expect("gate-identical error");
    assert!(error.contains("requires_buz"), "{error}");
    assert!(error.contains(">= 99.0.0"), "{error}");
    // Listed but not published → red with the download failure, persisted.
    let ghost = row("ghost");
    assert_eq!(ghost.buz_compatible, Some(false));
    let error = ghost.compat_error.as_deref().expect("file-missing error");
    assert!(error.contains("failed to download"), "{error}");
    assert!(error.contains("404"), "{error}");
}

#[tokio::test]
async fn mirror_endpoint_serves_sync_when_canonical_is_unreachable() {
    let yaml = community_spec();
    let yaml_sha = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(yaml.as_bytes()))
    };
    // The live server plays the mirror; the canonical origin is a closed
    // port, so every fetch must fall back to the mirror to succeed.
    let mirror_url = spawn_source_server(yaml.clone(), yaml_sha.clone()).await;
    let (app, pool) = test_app().await;
    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/admin/plugin-sources")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "alice",
                        "url": "http://127.0.0.1:1/index.json",
                        "mirror_url": format!("{mirror_url}/index.json"),
                        "auto_enable": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = read_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["source"]["mirror_url"],
        format!("{mirror_url}/index.json"),
        "{body}"
    );
    let source_id = body["data"]["source"]["id"]
        .as_str()
        .expect("source id")
        .to_string();

    // Deterministic sync (the same call the spawned background task makes):
    // index, reviews, and the template file all come from the mirror.
    let source = crate::services::plugin_pool::get_source(&pool, &source_id)
        .await
        .expect("get source")
        .expect("source exists");
    let http = crate::services::plugin_pool::http_client(30);
    let count =
        crate::services::plugin_pool::sync_source(&http, &pool, &source, shared::version::VERSION)
            .await
            .expect("sync must succeed through the mirror");
    assert!(count > 0, "mirror sync must stage pool rows");

    let refreshed = crate::services::plugin_pool::get_source(&pool, &source_id)
        .await
        .expect("get source")
        .expect("source exists");
    assert!(
        refreshed.last_sync_error.is_none(),
        "{:?}",
        refreshed.last_sync_error
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/admin/plugin-pool")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = read_json(resp).await;
    let steps = body["data"]["items"]
        .as_array()
        .expect("pool items")
        .iter()
        .filter(|row| row["namespace"] == "alice" && row["step_id"] == "oss-upload")
        .count();
    assert!(steps > 0, "pool rows staged from the mirror: {body}");
}

#[tokio::test]
async fn mirror_check_reports_in_sync_across_identical_servers() {
    let yaml = community_spec();
    let yaml_sha = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(yaml.as_bytes()))
    };
    let canonical_url = spawn_source_server(yaml.clone(), yaml_sha.clone()).await;
    let mirror_url = spawn_source_server(yaml, yaml_sha).await;
    let (app, pool) = test_app().await;
    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/admin/plugin-sources")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "alice",
                        "url": format!("{canonical_url}/index.json"),
                        "mirror_url": format!("{mirror_url}/index.json"),
                        "auto_enable": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = read_json(resp).await;
    assert_eq!(
        body["data"]["source"]["mirror_url"],
        format!("{mirror_url}/index.json"),
        "{body}"
    );
    let source_id = body["data"]["source"]["id"]
        .as_str()
        .expect("source id")
        .to_string();

    let report = crate::services::plugin_pool::check_mirror(
        &crate::services::plugin_pool::http_client(30),
        &pool,
        &source_id,
    )
    .await
    .expect("mirror check runs");
    assert_eq!(report.status, "in-sync", "{report:?}");
    assert_eq!(report.canonical_revision, report.mirror_revision);
    assert!(report.canonical_revision.is_some());
}

#[tokio::test]
async fn mirror_check_reports_out_of_sync_when_mirror_content_drifted() {
    let yaml = community_spec();
    let yaml_sha = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(yaml.as_bytes()))
    };
    // The mirror drifted: different spec bytes → different revision sentinel.
    let drifted = format!("{yaml}\n");
    let canonical_url = spawn_source_server(yaml, yaml_sha).await;
    let mirror_url = spawn_source_server(drifted, "9".repeat(64)).await;
    let (app, pool) = test_app().await;
    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/admin/plugin-sources")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "alice",
                        "url": format!("{canonical_url}/index.json"),
                        "mirror_url": format!("{mirror_url}/index.json"),
                        "auto_enable": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = read_json(resp).await;
    let source_id = body["data"]["source"]["id"]
        .as_str()
        .expect("source id")
        .to_string();

    let report = crate::services::plugin_pool::check_mirror(
        &crate::services::plugin_pool::http_client(30),
        &pool,
        &source_id,
    )
    .await
    .expect("mirror check runs");
    assert_eq!(report.status, "out-of-sync", "{report:?}");
    assert_ne!(report.canonical_revision, report.mirror_revision);
}

#[tokio::test]
async fn sync_error_includes_the_raw_response_when_index_is_not_json() {
    // A SPA fallback / wrong path: HTTP 200 with an HTML body.
    let app = Router::new().route(
        "/index.json",
        axum::routing::get(|| async { "<!doctype html><html><body>page</body></html>" }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let source_url = format!("http://{addr}/index.json");

    let (app, pool) = test_app().await;
    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap();
    let body = add_source(&app, &jwt, &source_url).await;
    let source_id = body["data"]["source"]["id"]
        .as_str()
        .expect("source id")
        .to_string();

    let source = crate::services::plugin_pool::get_source(&pool, &source_id)
        .await
        .expect("get source")
        .expect("source exists");
    let http = crate::services::plugin_pool::http_client(30);
    let error =
        crate::services::plugin_pool::sync_source(&http, &pool, &source, shared::version::VERSION)
            .await
            .unwrap_err()
            .to_string();
    assert!(error.contains("invalid index.json"), "{error}");
    assert!(error.contains("response body: <!doctype html>"), "{error}");
    assert!(error.contains("looks like HTML"), "{error}");
}

#[tokio::test]
async fn mirror_check_requires_a_configured_mirror() {
    let (app, pool) = test_app().await;
    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap();
    let body = add_source(&app, &jwt, "http://127.0.0.1:1/index.json").await;
    let source_id = body["data"]["source"]["id"]
        .as_str()
        .expect("source id")
        .to_string();

    let error = crate::services::plugin_pool::check_mirror(
        &crate::services::plugin_pool::http_client(30),
        &pool,
        &source_id,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("no mirror configured"), "{error}");
}

#[tokio::test]
async fn pool_toggle_writes_an_audit_record_with_the_diff() {
    let yaml = community_spec();
    let yaml_sha = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(yaml.as_bytes()))
    };
    let source_url = spawn_source_server(yaml, yaml_sha).await;
    let (app, pool) = test_app().await;
    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap();
    let body = add_source(&app, &jwt, &format!("{source_url}/index.json")).await;
    let source_id = body["data"]["source"]["id"]
        .as_str()
        .expect("source id")
        .to_string();
    // Deterministic sync so the pool row exists.
    let source = crate::services::plugin_pool::get_source(&pool, &source_id)
        .await
        .expect("get source")
        .expect("source exists");
    let http = crate::services::plugin_pool::http_client(30);
    crate::services::plugin_pool::sync_source(&http, &pool, &source, shared::version::VERSION)
        .await
        .expect("sync");

    // Enable via the API, then disable again.
    for enabled in [true, false] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/admin/plugin-pool/alice/oss-upload/1.0.0")
                    .header(AUTHORIZATION, format!("Bearer {jwt}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(format!(r#"{{ "enabled": {enabled} }}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let body = read_json(resp).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    let rows: Vec<(String, String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT action, resource_type, resource_id, before_json, after_json \
         FROM audit_logs WHERE action = 'plugin_pool.toggle' ORDER BY created_at",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "one record per toggle: {rows:?}");
    for (action, resource_type, resource_id, before, after) in &rows {
        assert_eq!(action, "plugin_pool.toggle");
        assert_eq!(resource_type, "plugin_pool");
        assert_eq!(resource_id, "alice/oss-upload@1.0.0");
        assert!(before.is_some() && after.is_some(), "{before:?} {after:?}");
    }
    // Curation provenance lands on the row itself (state), while the audit
    // log above carries the event stream.
    let provenance: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT updated_by, updated_at FROM plugin_pool WHERE step_id = 'oss-upload'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        provenance.0.as_deref(),
        Some("boss"),
        "last curator recorded"
    );
    assert!(provenance.1.is_some(), "timestamp recorded");

    // The diff states what changed: enable went false→true, disable true→false.
    let parsed: Vec<(serde_json::Value, serde_json::Value)> = rows
        .iter()
        .map(|(_, _, _, before, after)| {
            (
                serde_json::from_str(before.as_deref().unwrap()).unwrap(),
                serde_json::from_str(after.as_deref().unwrap()).unwrap(),
            )
        })
        .collect();
    assert_eq!(parsed[0].0["enabled"], false);
    assert_eq!(parsed[0].1["enabled"], true);
    assert_eq!(parsed[1].0["enabled"], true);
    assert_eq!(parsed[1].1["enabled"], false);
}

/// Download policy: a full-meta index declares `requires_buz`, so a sync only
/// fetches the newest version of each plugin. Older versions stage with an
/// unknown verdict and are fetched when someone enables or pins them. This
/// keeps first-sync traffic proportional to plugins, not to plugin-versions.
#[tokio::test]
async fn sync_downloads_only_the_newest_version_when_the_index_declares_requires_buz() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // The template cache is keyed by content hash and lives on disk, so a
    // fixed body would be served from cache on the second run and this mock
    // server would never see a request. A per-run nonce keeps the assertion
    // deterministic.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let spec = |version: &str| {
        format!(
            r#"id: car/tool
# download-policy fixture nonce: {nonce}
version: {version}
publisher: community
author: carol
source_repo: https://github.com/carol/steps
engine: shell
requires_buz: ">=1.0.0"
inputs:
  name:
    type: string
    required: true
run: |
  echo "hello ${{INPUT_NAME}}"
"#
        )
    };
    let sha = |raw: &str| {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(raw.as_bytes()))
    };

    let versions = ["1.0.0", "1.1.0", "2.0.0"];
    let specs: Vec<String> = versions.iter().map(|v| spec(v)).collect();
    let index = format!(
        r#"{{ "steps": [ {} ] }}"#,
        versions
            .iter()
            .zip(&specs)
            .map(|(version, body)| format!(
                r#"{{ "id": "car/tool", "version": "{version}",
                   "file": "steps/tool/{version}.yaml", "sha256": "{}",
                   "requires_buz": ">=1.0.0" }}"#,
                sha(body)
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let fetched: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let mut app = Router::new().route(
        "/index.json",
        axum::routing::get(move || {
            let index = index.clone();
            async move { index }
        }),
    );
    for (version, body) in versions.iter().zip(&specs) {
        let counter = fetched.clone();
        let body = body.clone();
        app = app.route(
            format!("/steps/tool/{version}.yaml").as_str(),
            axum::routing::get(move || {
                let counter = counter.clone();
                let body = body.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    body
                }
            }),
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");
    sqlx::query(
        "INSERT INTO plugin_sources (id, name, url, auto_enable, enabled) \
         VALUES ('src-newest', 'newest', ?, 0, 1)",
    )
    .bind(format!("http://{addr}/index.json"))
    .execute(&pool)
    .await
    .unwrap();
    let source = crate::services::plugin_pool::get_source(&pool, "src-newest")
        .await
        .unwrap()
        .expect("source row");
    let http = crate::services::plugin_pool::http_client(60);
    crate::services::plugin_pool::sync_source(&http, &pool, &source, shared::version::VERSION)
        .await
        .expect("sync");

    assert_eq!(
        fetched.load(Ordering::SeqCst),
        1,
        "only the newest version is downloaded at sync time"
    );

    let verdicts: Vec<(String, Option<i64>)> =
        sqlx::query_as("SELECT version, buz_compatible FROM plugin_pool ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        verdicts,
        vec![
            ("1.0.0".to_string(), None),
            ("1.1.0".to_string(), None),
            ("2.0.0".to_string(), Some(1)),
        ],
        "newest verified, older versions stage as unknown"
    );

    // Enabling an older version is the "user asked for it" path: it verifies
    // on demand (cache miss → download).
    crate::services::plugin_pool::verify_entry(
        &http,
        &pool,
        "car",
        "tool",
        "1.0.0",
        shared::version::VERSION,
    )
    .await
    .expect("older version verifies on demand");
    assert_eq!(
        fetched.load(Ordering::SeqCst),
        2,
        "the requested version was downloaded when enabled"
    );
}
