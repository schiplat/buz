use super::*;

#[test]
fn log_entry_json_includes_created_at() {
    let entry = LogEntry::new(0, "hello\n");
    let v: serde_json::Value = serde_json::from_str(&entry.to_json_string()).unwrap();
    assert_eq!(v["step_order"], 0);
    assert_eq!(v["content"], "hello\n");
    let ts = v["created_at"].as_str().unwrap();
    assert!(ts.contains('T'), "expected RFC3339, got {ts}");
    assert!(ts.ends_with('Z'), "expected UTC Z, got {ts}");
}
