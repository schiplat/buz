use plugin_cli::release_check::{check_published, compare_entries, compare_header, meta_errors};
use serde_json::{json, Value};
use std::path::Path;

fn sha(char: char) -> String {
    char.to_string().repeat(64)
}

fn entry(version: &str, sha: &str) -> Value {
    json!({
        "id": "buz/a",
        "version": version,
        "file": format!("buz/a/{version}.yaml"),
        "sha256": sha,
        "requires_buz": ">=1.8.1"
    })
}

fn index(steps: Vec<Value>, sequence: u64, issued_at: &str) -> Value {
    json!({ "sequence": sequence, "issued_at": issued_at, "steps": steps })
}

fn write_meta(root: &Path, committed: &Value, mutate: impl Fn(&mut Value)) {
    let mut groups: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
    for step in committed["steps"].as_array().unwrap() {
        let file = step["file"].as_str().unwrap();
        let parent = Path::new(file)
            .parent()
            .unwrap()
            .to_string_lossy()
            .to_string();
        groups.entry(parent).or_default().push(step.clone());
    }
    for (rel, steps) in groups {
        let mut versions: Vec<Value> = steps
            .iter()
            .map(|step| {
                let mut clone = step.as_object().unwrap().clone();
                clone.remove("id");
                Value::Object(clone)
            })
            .collect();
        versions.sort_by_key(|step| step["version"].as_str().unwrap().to_string());
        let mut meta = json!({ "id": steps[0]["id"], "versions": versions });
        mutate(&mut meta);
        let path = root.join(&rel).join("meta.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_string(&meta).unwrap()).unwrap();
    }
}

#[test]
fn identical_catalog_passes() {
    let committed = index(vec![entry("1.0.0", &sha('a'))], 4, "2026-11-09T10:22:49Z");
    assert!(compare_entries(&committed, &committed).unwrap().is_empty());
    assert!(compare_header(&committed, &committed).is_empty());
}

#[test]
fn a_dropped_field_names_the_field_and_version() {
    let committed = index(
        vec![json!({
            "id": "buz/a", "version": "1.0.0", "file": "buz/a/1.0.0.yaml",
            "sha256": sha('a'), "deprecated": "Pin 1.1.1 instead"
        })],
        4,
        "2026-11-09T10:22:49Z",
    );
    let generated = index(vec![entry("1.0.0", &sha('a'))], 4, "2026-11-09T10:22:49Z");
    let errors = compare_entries(&committed, &generated).unwrap();
    let joined = errors.join("\n");
    assert!(joined.contains("buz/a@1.0.0"));
    assert!(joined.contains("`deprecated`"));
    assert!(joined.contains("Pin 1.1.1 instead"));
}

#[test]
fn an_implicit_sequence_advance_is_refused() {
    let committed = index(vec![entry("1.0.0", &sha('a'))], 4, "2026-11-09T10:22:49Z");
    let generated = index(vec![entry("1.0.0", &sha('a'))], 5, "2026-11-09T10:22:49Z");
    let errors = compare_header(&committed, &generated);
    assert!(errors.iter().any(|error| error.contains("--sequence")));
}

#[test]
fn an_issued_at_divergence_is_refused() {
    let committed = index(vec![entry("1.0.0", &sha('a'))], 4, "2026-11-09T10:22:49Z");
    let generated = index(vec![entry("1.0.0", &sha('a'))], 4, "2026-11-10T02:36:34Z");
    let errors = compare_header(&committed, &generated);
    assert!(errors.iter().any(|error| error.contains("--updated-at")));
}

#[test]
fn a_catalog_behind_the_published_release_is_refused() {
    let committed = index(vec![entry("1.0.0", &sha('a'))], 4, "2026-11-09T10:22:49Z");
    let published = index(vec![entry("1.0.0", &sha('a'))], 5, "2026-11-10T02:36:34Z");
    let errors = check_published(&committed, &published);
    let joined = errors.join("\n");
    assert!(joined.contains("behind the published release"));
    assert!(joined.contains("--sequence 6"));
}

#[test]
fn the_same_sequence_with_new_content_is_refused() {
    let committed = index(vec![entry("1.0.0", &sha('b'))], 5, "2026-11-10T02:36:34Z");
    let published = index(vec![entry("1.0.0", &sha('a'))], 5, "2026-11-10T02:36:34Z");
    let errors = check_published(&committed, &published);
    assert!(errors
        .join("\n")
        .contains("two catalogs under one sequence"));
}

#[test]
fn an_idempotent_republish_and_a_forward_move_pass() {
    let published = index(vec![entry("1.0.0", &sha('a'))], 5, "2026-11-10T02:36:34Z");
    assert!(check_published(&published, &published).is_empty());

    let ahead = index(vec![entry("1.0.0", &sha('a'))], 6, "2026-11-11T00:00:00Z");
    assert!(check_published(&ahead, &published).is_empty());
}

#[test]
fn meta_projection_passes_when_it_matches_the_committed_index() {
    let dir = tempfile::tempdir().unwrap();
    let committed = index(
        vec![entry("1.0.0", &sha('a')), entry("1.1.0", &sha('b'))],
        4,
        "2026-11-09T10:22:49Z",
    );
    write_meta(dir.path(), &committed, |_| {});
    assert!(meta_errors(&committed, dir.path()).unwrap().is_empty());
}

#[test]
fn a_dropped_meta_field_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let committed = index(
        vec![json!({
            "id": "buz/a", "version": "1.0.0", "file": "buz/a/1.0.0.yaml",
            "sha256": sha('a'), "deprecated": "Pin 1.1.1 instead"
        })],
        4,
        "2026-11-09T10:22:49Z",
    );
    write_meta(dir.path(), &committed, |meta| {
        meta["versions"][0]
            .as_object_mut()
            .unwrap()
            .remove("deprecated");
    });
    let errors = meta_errors(&committed, dir.path()).unwrap();
    assert!(errors.iter().any(|error| error.contains("`deprecated`")));
    assert!(errors
        .iter()
        .any(|error| error.contains("Pin 1.1.1 instead")));
}

#[test]
fn a_missing_or_extended_meta_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let committed = index(vec![entry("1.0.0", &sha('a'))], 4, "2026-11-09T10:22:49Z");
    let errors = meta_errors(&committed, dir.path()).unwrap();
    assert!(errors.iter().any(|error| error.contains("missing")));

    let dir = tempfile::tempdir().unwrap();
    write_meta(dir.path(), &committed, |meta| {
        let mut extra = meta["versions"][0].clone();
        extra["version"] = json!("9.9.9");
        meta["versions"].as_array_mut().unwrap().push(extra);
    });
    let errors = meta_errors(&committed, dir.path()).unwrap();
    assert!(errors.iter().any(|error| error.contains("9.9.9")));
}
