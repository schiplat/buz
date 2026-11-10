use plugin_cli::revision::{catalog_revision, run, Options};
use serde_json::{json, Value};
use std::path::Path;

fn sha(char: char) -> String {
    char.to_string().repeat(64)
}

fn index(steps: Vec<Value>, sequence: u64, issued_at: &str) -> Value {
    json!({ "sequence": sequence, "issued_at": issued_at, "steps": steps })
}

fn entry(id: &str, version: &str, sha: &str) -> Value {
    json!({ "id": id, "version": version, "sha256": sha })
}

fn write_index(dir: &Path, value: &Value) -> std::path::PathBuf {
    let path = dir.join("index.json");
    std::fs::write(&path, serde_json::to_string(value).unwrap()).unwrap();
    path
}

#[test]
fn derivation_is_stable_across_build_metadata_and_entry_order() {
    let a = entry("buz/a", "1.0.0", &sha('a'));
    let b = entry("buz/b", "2.0.0", &sha('b'));
    let one = catalog_revision(&index(
        vec![a.clone(), b.clone()],
        3,
        "2026-01-01T00:00:00Z",
    ))
    .unwrap();
    let two = catalog_revision(&index(vec![b, a], 99, "2030-12-31T23:59:59Z")).unwrap();
    assert_eq!(
        one, two,
        "build metadata and entry order must not move the sentinel"
    );
    assert_eq!(one.len(), 16);
    assert!(one.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn derivation_changes_only_when_the_catalog_changes() {
    let base = vec![entry("buz/a", "1.0.0", &sha('a'))];
    let baseline = catalog_revision(&index(base.clone(), 1, "2026-01-01T00:00:00Z")).unwrap();
    for variant in [
        vec![entry("buz/a", "1.0.0", &sha('b'))],
        vec![entry("buz/a", "1.0.1", &sha('a'))],
        vec![entry("buz/c", "1.0.0", &sha('a'))],
        {
            let mut steps = base.clone();
            steps.push(entry("buz/a", "1.0.1", &sha('b')));
            steps
        },
    ] {
        assert_ne!(
            baseline,
            catalog_revision(&index(variant, 1, "2026-01-01T00:00:00Z")).unwrap(),
            "sentinel did not move for a catalog change"
        );
    }
}

#[test]
fn write_mode_records_the_index_issued_at_and_print_mode_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let value = index(
        vec![entry("buz/a", "1.0.0", &sha('a'))],
        4,
        "2026-02-03T04:05:06Z",
    );
    let index_path = write_index(dir.path(), &value);
    let output = dir.path().join("revision.json");
    run(&Options {
        index: index_path.clone(),
        output: output.clone(),
        print_only: false,
    })
    .unwrap();
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(written["revision"], catalog_revision(&value).unwrap());
    assert_eq!(written["updated_at"], "2026-02-03T04:05:06Z");

    let other = dir.path().join("printed-only.json");
    run(&Options {
        index: index_path,
        output: other.clone(),
        print_only: true,
    })
    .unwrap();
    assert!(!other.exists(), "--print must not write");
}

#[test]
fn an_index_without_steps_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.json");
    std::fs::write(&path, r#"{"sequence":1}"#).unwrap();
    let error = run(&Options {
        index: path,
        output: dir.path().join("revision.json"),
        print_only: true,
    })
    .unwrap_err();
    assert!(format!("{error:#}").contains("steps"));
}
