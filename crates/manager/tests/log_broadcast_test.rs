use manager::services::log_broadcast::*;

#[test]
fn log_entry_json_includes_created_at() {
    let entry = LogEntry::new(0, Some("error"), "hello\n");
    let v: serde_json::Value = serde_json::from_str(&entry.to_json_string()).unwrap();
    assert_eq!(v["step_order"], 0);
    assert_eq!(v["content"], "hello\n");
    assert_eq!(v["level"], "error");
    let ts = v["created_at"].as_str().unwrap();
    assert!(ts.contains('T'), "expected RFC3339, got {ts}");
    assert!(ts.ends_with('Z'), "expected UTC Z, got {ts}");
}

#[test]
fn log_entry_level_is_null_for_legacy_chunks() {
    let entry = LogEntry::new(3, None, "legacy");
    let v: serde_json::Value = serde_json::from_str(&entry.to_json_string()).unwrap();
    assert!(v["level"].is_null(), "{v}");
}
