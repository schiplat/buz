use super::merge_hook_inputs;

#[test]
fn environment_selector_is_reserved_not_a_business_input() {
    let mut query = std::collections::HashMap::new();
    query.insert("environment".into(), "production".into());
    query.insert("version".into(), "1.0".into());
    let body = serde_json::json!({
        "environment": "staging",
        "inputs": { "environment": "development", "version": "2.0" }
    });
    let result = merge_hook_inputs(&query, &body);
    assert!(!result.contains_key("environment"));
    assert_eq!(result.get("version").map(String::as_str), Some("2.0"));
}

#[test]
fn merge_empty() {
    let query = std::collections::HashMap::new();
    let body = serde_json::json!({});
    let result = merge_hook_inputs(&query, &body);
    assert!(result.is_empty());
}

#[test]
fn merge_query_only() {
    let mut query = std::collections::HashMap::new();
    query.insert("version".into(), "1.0".into());
    let body = serde_json::json!({});
    let result = merge_hook_inputs(&query, &body);
    assert_eq!(result.get("version").map(|s| s.as_str()), Some("1.0"));
}

#[test]
fn merge_body_inputs_override_query() {
    let mut query = std::collections::HashMap::new();
    query.insert("version".into(), "1.0".into());
    query.insert("env".into(), "dev".into());
    let body = serde_json::json!({"inputs": {"version": "2.0"}});
    let result = merge_hook_inputs(&query, &body);
    assert_eq!(result.get("version").map(|s| s.as_str()), Some("2.0"));
    assert_eq!(result.get("env").map(|s| s.as_str()), Some("dev"));
}

#[test]
fn merge_flat_body_keys() {
    let query = std::collections::HashMap::new();
    let body = serde_json::json!({"version": "3.0", "env": "staging"});
    let result = merge_hook_inputs(&query, &body);
    assert_eq!(result.get("version").map(|s| s.as_str()), Some("3.0"));
    assert_eq!(result.get("env").map(|s| s.as_str()), Some("staging"));
}

#[test]
fn merge_flat_body_does_not_override_query() {
    let mut query = std::collections::HashMap::new();
    query.insert("version".into(), "1.0".into());
    let body = serde_json::json!({"version": "2.0"});
    let result = merge_hook_inputs(&query, &body);
    assert_eq!(result.get("version").map(|s| s.as_str()), Some("1.0"));
}

#[test]
fn merge_inputs_priority_highest() {
    let mut query = std::collections::HashMap::new();
    query.insert("version".into(), "q".into());
    let body = serde_json::json!({"version": "f", "inputs": {"version": "i"}});
    let result = merge_hook_inputs(&query, &body);
    assert_eq!(result.get("version").map(|s| s.as_str()), Some("i"));
}

#[test]
fn merge_ignores_non_string_json_values() {
    let query = std::collections::HashMap::new();
    let body = serde_json::json!({
        "inputs": {"ok": "yes", "num": 1, "flag": true, "obj": {"a": 1}},
        "flat_num": 42,
        "flat_ok": "keep"
    });
    let result = merge_hook_inputs(&query, &body);
    assert_eq!(result.get("ok").map(|s| s.as_str()), Some("yes"));
    assert_eq!(result.get("flat_ok").map(|s| s.as_str()), Some("keep"));
    assert!(!result.contains_key("num"));
    assert!(!result.contains_key("flag"));
    assert!(!result.contains_key("obj"));
    assert!(!result.contains_key("flat_num"));
}

#[test]
fn merge_empty_inputs_object_preserves_query() {
    let mut query = std::collections::HashMap::new();
    query.insert("version".into(), "1.0".into());
    let body = serde_json::json!({"inputs": {}});
    let result = merge_hook_inputs(&query, &body);
    assert_eq!(result.get("version").map(|s| s.as_str()), Some("1.0"));
}
