use manager::services::platform_settings::*;

#[test]
fn validate_bool_rejects_junk() {
    assert!(validate_value(KEY_PLUGIN_REVIEW_REQUIRED, "true").is_ok());
    assert!(validate_value(KEY_PLUGIN_REVIEW_REQUIRED, "yes").is_err());
    assert!(validate_value(KEY_PLUGIN_REVIEW_REQUIRED, "").is_err());
}

#[test]
fn validate_sync_timeout_zero_is_explicit_disable() {
    // 0 = timeout disabled (reqwest semantics) — legal but dangerous,
    // the UI flags it loudly.
    assert!(validate_value(KEY_PLUGIN_SYNC_TIMEOUT, "0").is_ok());
    assert!(validate_value(KEY_PLUGIN_SYNC_TIMEOUT, "60").is_ok());
    assert!(validate_value(KEY_PLUGIN_SYNC_TIMEOUT, "300").is_ok());
    assert!(validate_value(KEY_PLUGIN_SYNC_TIMEOUT, "301").is_err());
    assert!(validate_value(KEY_PLUGIN_SYNC_TIMEOUT, "4").is_err());
}

#[test]
fn validate_url_allows_empty_reset_and_rejects_non_http() {
    assert!(validate_value(KEY_MARKETPLACE_URL, "").is_ok());
    assert!(validate_value(KEY_MARKETPLACE_URL, "https://plugins.example.com").is_ok());
    assert!(validate_value(KEY_MARKETPLACE_URL, "ftp://x").is_err());
    assert!(validate_value(KEY_MARKETPLACE_URL, "not a url").is_err());
}

#[test]
fn validate_path_rejects_empty() {
    assert!(validate_value(KEY_PLUGIN_INDEX_LOCAL_DIR, "/srv/steps").is_ok());
    assert!(validate_value(KEY_PLUGIN_INDEX_LOCAL_DIR, "  ").is_err());
}

#[test]
fn unknown_key_is_rejected() {
    assert!(validate_value("nope", "x").is_err());
    assert!(setting_kind("nope").is_none());
}
