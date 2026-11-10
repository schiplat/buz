use plugin_cli::manifest_check::{check, run, Options};
use plugin_cli::revision::catalog_revision;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn sha(char: char) -> String {
    char.to_string().repeat(64)
}

fn step(version: &str, sha: &str) -> Value {
    json!({
        "id": "buz/a",
        "version": version,
        "file": format!("buz/a/{version}.yaml"),
        "sha256": sha
    })
}

fn review(version: &str, sha: &str) -> Value {
    json!({
        "id": "buz/a",
        "version": version,
        "sha256": sha,
        "reviewed_at": "2026-01-01",
        "review_commit": "abc1234",
        "scope": "static-lint + manual",
        "source": "ci"
    })
}

fn build(root: &Path, index: &Value, reviews: &Value) -> PathBuf {
    let plugins = root.join("plugins");
    let dir = plugins.join("buz/a");
    std::fs::create_dir_all(&dir).unwrap();
    let index = index.clone();
    std::fs::write(
        plugins.join("index.json"),
        serde_json::to_string(&index).unwrap(),
    )
    .unwrap();
    std::fs::write(
        plugins.join("reviews.json"),
        serde_json::to_string(reviews).unwrap(),
    )
    .unwrap();
    let revision = catalog_revision(&index).unwrap();
    std::fs::write(
        plugins.join("revision.json"),
        serde_json::to_string(&json!({
            "revision": revision,
            "updated_at": index["issued_at"]
        }))
        .unwrap(),
    )
    .unwrap();
    let versions: Vec<Value> = index["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| {
            let mut entry = step.as_object().unwrap().clone();
            entry.remove("id");
            Value::Object(entry)
        })
        .collect();
    std::fs::write(
        dir.join("meta.json"),
        serde_json::to_string(&json!({ "id": "buz/a", "versions": versions })).unwrap(),
    )
    .unwrap();
    plugins
}

fn index(steps: Vec<Value>) -> Value {
    json!({ "sequence": 1, "issued_at": "2026-01-01T00:00:00Z", "steps": steps })
}

fn reviews(entries: Vec<Value>) -> Value {
    json!({ "reviews": entries })
}

#[test]
fn consistent_manifests_pass() {
    let dir = tempfile::tempdir().unwrap();
    let index = index(vec![step("1.0.0", &sha('a'))]);
    let plugins = build(
        dir.path(),
        &index,
        &reviews(vec![review("1.0.0", &sha('a'))]),
    );
    run(&Options {
        plugins_dir: plugins,
    })
    .unwrap();
}

#[test]
fn a_missing_review_is_refused_with_the_refresh_hint() {
    let dir = tempfile::tempdir().unwrap();
    let index = index(vec![step("1.0.0", &sha('a'))]);
    let plugins = build(dir.path(), &index, &reviews(vec![]));
    let (errors, _) = check(&index, &reviews(vec![])).unwrap();
    assert!(errors.iter().any(|error| error.contains("no review entry")));
    assert!(errors
        .iter()
        .any(|error| error.contains("buz-plugin reviews")));
    assert!(run(&Options {
        plugins_dir: plugins
    })
    .is_err());
}

#[test]
fn a_review_bound_to_older_content_is_refused() {
    let index = index(vec![step("1.0.0", &sha('a'))]);
    let ledger = reviews(vec![review("1.0.0", &sha('b'))]);
    let (errors, _) = check(&index, &ledger).unwrap();
    assert!(errors
        .iter()
        .any(|error| error.contains("content changed after review")));
}

#[test]
fn a_stale_sentinel_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let index = index(vec![step("1.0.0", &sha('a'))]);
    let plugins = build(
        dir.path(),
        &index,
        &reviews(vec![review("1.0.0", &sha('a'))]),
    );
    std::fs::write(
        plugins.join("revision.json"),
        r#"{"revision":"0123456789abcdef"}"#,
    )
    .unwrap();
    let error = run(&Options {
        plugins_dir: plugins,
    })
    .unwrap_err();
    assert!(format!("{error:#}").contains("inconsistency"));
}

#[test]
fn a_meta_hash_mismatch_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let index = index(vec![step("1.0.0", &sha('a'))]);
    let plugins = build(
        dir.path(),
        &index,
        &reviews(vec![review("1.0.0", &sha('a'))]),
    );
    std::fs::write(
        plugins.join("buz/a/meta.json"),
        serde_json::to_string(&json!({
            "id": "buz/a",
            "versions": [{"version": "1.0.0", "file": "buz/a/1.0.0.yaml", "sha256": sha('b')}]
        }))
        .unwrap(),
    )
    .unwrap();
    let error = run(&Options {
        plugins_dir: plugins,
    })
    .unwrap_err();
    assert!(format!("{error:#}").contains("inconsistency"));
}

#[test]
fn duplicate_index_entries_are_refused() {
    let index = index(vec![step("1.0.0", &sha('a')), step("1.0.0", &sha('b'))]);
    let ledger = reviews(vec![review("1.0.0", &sha('a'))]);
    let (errors, _) = check(&index, &ledger).unwrap();
    assert!(errors
        .iter()
        .any(|error| error.contains("duplicate index entry")));
}

#[test]
fn a_review_without_an_index_version_is_only_a_warning() {
    let index = index(vec![step("1.0.0", &sha('a'))]);
    let ledger = reviews(vec![review("1.0.0", &sha('a')), review("0.9.0", &sha('c'))]);
    let (errors, warnings) = check(&index, &ledger).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    assert!(warnings
        .iter()
        .any(|warning| warning.contains("no matching index version")));
}
