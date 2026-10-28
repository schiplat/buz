use manager::services::plugin_pool::*;

fn index_json(steps: serde_json::Value) -> String {
    serde_json::json!({ "steps": steps }).to_string()
}

fn entry(id: &str, sha256: &str) -> serde_json::Value {
    serde_json::json!({ "id": id, "version": "1.0.0", "file": "x.yaml", "sha256": sha256 })
}

#[test]
fn parse_accepts_single_namespace_index() {
    let raw = index_json(serde_json::json!([
        entry("buz/archive", &"a".repeat(64)),
        entry("buz/checkout", &"b".repeat(64)),
    ]));
    let index = parse_index(&raw).expect("single-namespace index parses");
    assert_eq!(index.steps.len(), 2);
}

#[test]
fn parse_rejects_mixed_namespace_index() {
    let raw = index_json(serde_json::json!([
        entry("buz/archive", &"a".repeat(64)),
        entry("alice/upload", &"b".repeat(64)),
    ]));
    let error = parse_index(&raw).unwrap_err();
    assert!(
        error.contains("mixes namespaces 'buz' and 'alice'"),
        "{error}"
    );
}

#[test]
fn parse_rejects_ids_without_namespace() {
    let raw = index_json(serde_json::json!([entry("archive", &"a".repeat(64))]));
    let error = parse_index(&raw).unwrap_err();
    assert!(error.contains("'<namespace>/<plugin>'"), "{error}");
}
