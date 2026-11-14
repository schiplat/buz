//! `GET/PUT/DELETE /api/v1/admin/settings` — listing, override application,
//! validation and reset. Converted to the shared dual-backend fixture: the
//! same journeys run on SQLite (default) and, with `BUZ_TEST_DB=pg`, on
//! Postgres.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use manager::services::plugin_resolver;
use manager::store::exec;

/// The plugin settings API applies overrides to a PROCESS-GLOBAL resolver
/// slot (recomputed as a full set from the calling test's database), so two
/// tests in this binary racing `apply_runtime_overrides` can clobber each
/// other. Serialize every test here — each runs alone.
static SETTINGS_GLOBAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn get_settings(app: &TestApp, token: &str) -> (StatusCode, serde_json::Value) {
    app.get("/api/v1/admin/settings", Some(token)).await
}

async fn put_setting(
    app: &TestApp,
    token: &str,
    key: &str,
    value: &str,
) -> (StatusCode, serde_json::Value) {
    app.put(
        &format!("/api/v1/admin/settings/{key}"),
        Some(token),
        serde_json::json!({ "value": value }),
    )
    .await
}

async fn delete_setting(app: &TestApp, token: &str, key: &str) -> StatusCode {
    let (status, _) = app
        .delete(&format!("/api/v1/admin/settings/{key}"), Some(token))
        .await;
    status
}

fn setting_value(body: &serde_json::Value, key: &str) -> String {
    body["data"]["items"]
        .as_array()
        .expect("items array")
        .iter()
        .find(|item| item["key"] == key)
        .expect("setting present")["value"]
        .as_str()
        .expect("string value")
        .to_string()
}

fn data_ok(body: &serde_json::Value) -> bool {
    body["code"] == 0 && body["data"].is_object()
}

#[tokio::test]
async fn settings_require_platform_admin() {
    let _global = SETTINGS_GLOBAL.lock().await;
    let app = TestApp::boot().await;

    let (status, _) = get_settings(&app, &app.member_jwt()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = put_setting(&app, &app.member_jwt(), "plugin_sync_timeout", "60").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn settings_defaults_come_from_config() {
    let _global = SETTINGS_GLOBAL.lock().await;
    let app = TestApp::boot().await;
    let token = app.admin_jwt();
    let (status, body) = get_settings(&app, &token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(data_ok(&body));
    assert_eq!(setting_value(&body, "plugin_review_required"), "false");
    // local_dir default resolves to an ABSOLUTE path under the DATA root —
    // operators never see a bare relative path or a pipeline-scoped one.
    let expected = std::env::current_dir()
        .unwrap()
        .join("data/plugins")
        .to_string_lossy()
        .into_owned();
    assert_eq!(setting_value(&body, "plugin_index_local_dir"), expected);
    // marketplace_url is no longer an admin settings item (navigation link,
    // consumed via /health): absent from the list entirely.
    let keys: Vec<&str> = body["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["key"].as_str())
        .collect();
    assert!(!keys.contains(&"marketplace_url"), "{keys:?}");
}

/// The approval switches are listed, writable, and resettable. The UI writes
/// both keys together (one "Require approvals" toggle); the API keeps them
/// independently settable, so each is asserted on its own.
#[tokio::test]
async fn approval_switches_list_write_and_reset() {
    let _global = SETTINGS_GLOBAL.lock().await;
    let app = TestApp::boot().await;
    let token = app.admin_jwt();

    // Listed with a false default.
    let (status, body) = get_settings(&app, &token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        setting_value(&body, "sandbox_config_require_approval"),
        "false"
    );
    assert_eq!(setting_value(&body, "grants_require_approval"), "false");

    // Each key flips on its own — API-level granularity the UI chooses not
    // to expose.
    let (status, _) = put_setting(&app, &token, "grants_require_approval", "true").await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = get_settings(&app, &token).await;
    assert_eq!(setting_value(&body, "grants_require_approval"), "true");
    assert_eq!(
        setting_value(&body, "sandbox_config_require_approval"),
        "false"
    );

    // Reset falls back to the default.
    assert_eq!(
        delete_setting(&app, &token, "grants_require_approval").await,
        StatusCode::OK
    );
    let (_, body) = get_settings(&app, &token).await;
    assert_eq!(setting_value(&body, "grants_require_approval"), "false");
}

#[tokio::test]
async fn update_persists_applies_and_reset_falls_back() {
    let _global = SETTINGS_GLOBAL.lock().await;
    let app = TestApp::boot().await;
    let token = app.admin_jwt();

    // The gate cannot be enabled while the manifest file is missing — the
    // refusal is asserted first, then a manifest is provisioned into this
    // test's OWN data dir (parallel tests must not see each other's files)
    // and the enable succeeds.
    let (status, body) = put_setting(&app, &token, "plugin_review_required", "true").await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "enable without a manifest must be refused: {body}"
    );
    // Provision a manifest into a per-test tempdir and point the manifest
    // setting at it (the guard resolves that override).
    let data_dir = tempfile::tempdir().unwrap();
    let manifest_path = data_dir.path().join("reviews.json");
    std::fs::write(&manifest_path, r#"{"reviews": []}"#).unwrap();
    let manifest = manifest_path.to_string_lossy().into_owned();
    let (status, body) = put_setting(&app, &token, "plugin_reviewed_manifest", &manifest).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // NOW the enable passes the guard (manifest override resolves to the
    // tempdir file) and flips the resolver.
    let (status, body) = put_setting(&app, &token, "plugin_review_required", "true").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(data_ok(&body));
    let policy = plugin_resolver::policy_from_settings();
    assert!(policy.require_reviewed, "override must reach the resolver");

    let row = exec::fetch_optional(
        &app.pool,
        "SELECT value FROM platform_settings WHERE key = 'plugin_review_required'",
        &[],
    )
    .await
    .expect("override row")
    .expect("override row present");
    assert_eq!(row.text(0), "true");

    // Invalid value is rejected with a validation error and nothing persisted.
    let (status, _) = put_setting(&app, &token, "plugin_review_required", "yes").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Reset clears the override; the resolver falls back to config (false).
    assert_eq!(
        delete_setting(&app, &token, "plugin_review_required").await,
        StatusCode::OK
    );
    let policy = plugin_resolver::policy_from_settings();
    assert!(!policy.require_reviewed, "reset must fall back to config");
}

#[tokio::test]
async fn unknown_setting_and_invalid_path_are_rejected() {
    let _global = SETTINGS_GLOBAL.lock().await;
    let app = TestApp::boot().await;
    let token = app.admin_jwt();

    // marketplace_url remains a REGISTERED key (env/toml override consumed
    // via /health) — writable but not listed in the settings UI. A genuinely
    // unknown name is the 422 probe.
    let (status, _) = put_setting(&app, &token, "no-such-key", "x").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = put_setting(&app, &token, "plugin_index_local_dir", "   ").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
