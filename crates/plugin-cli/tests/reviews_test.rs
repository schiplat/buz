use plugin_cli::reviews::{run, Options};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn step(id: &str, version: &str) -> Value {
    json!({
        "id": id,
        "version": version,
        "file": format!("{}/{version}.yaml", id),
        "sha256": format!("{id}-{version}").chars().cycle().take(64).collect::<String>(),
        "category": "config"
    })
}

fn source_root(dir: &Path, reviews: Option<Value>) -> PathBuf {
    let plugins = dir.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    let index = json!({
        "sequence": 4,
        "issued_at": "2026-01-01T00:00:00Z",
        "steps": [
            step("buz/a", "1.0.0"),
            step("buz/a", "1.1.0"),
            step("buz/new", "1.0.0"),
            step("alice/x", "1.0.0")
        ]
    });
    std::fs::write(
        plugins.join("index.json"),
        serde_json::to_string(&index).unwrap(),
    )
    .unwrap();
    if let Some(ledger) = reviews {
        std::fs::write(
            plugins.join("reviews.json"),
            serde_json::to_string(&ledger).unwrap(),
        )
        .unwrap();
    }
    plugins
}

fn human(version: &str) -> Value {
    json!({
        "id": "buz/a",
        "version": version,
        "sha256": "x".repeat(64),
        "reviewed_at": "2026-01-01",
        "review_commit": "deadbee",
        "scope": "static-lint + manual",
        "source": "human"
    })
}

fn read_ledger(plugins: &Path) -> Vec<Value> {
    let data: Value =
        serde_json::from_str(&std::fs::read_to_string(plugins.join("reviews.json")).unwrap())
            .unwrap();
    data["reviews"].as_array().unwrap().clone()
}

fn options(plugins: &Path, auto: bool) -> Options {
    Options {
        plugins_dir: plugins.to_path_buf(),
        auto,
        date: Some("2026-11-10".into()),
        commit: Some("abc1234".into()),
        scope: "static-lint + manual".into(),
        ids: Vec::new(),
    }
}

#[test]
fn auto_refreshes_blessed_versions_and_keeps_the_human_first_review() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = source_root(dir.path(), Some(json!({ "reviews": [human("1.0.0")] })));
    run(&options(&plugins, true)).unwrap();

    let entries = read_ledger(&plugins);
    let pairs: Vec<(String, String, String)> = entries
        .iter()
        .map(|entry| {
            (
                entry["id"].as_str().unwrap().to_string(),
                entry["version"].as_str().unwrap().to_string(),
                entry["source"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("buz/a".into(), "1.0.0".into(), "ci".into()),
            ("buz/a".into(), "1.1.0".into(), "ci".into()),
            ("buz/a".into(), "1.0.0".into(), "human".into()),
        ],
        "ci badge and human first review coexist; only blessed ids are refreshed"
    );
    assert!(
        entries.iter().all(|entry| entry["id"] != "buz/new"),
        "a brand-new official id is never auto-blessed"
    );
}

#[test]
fn manual_mode_picks_the_latest_version_and_supersedes_that_pair() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = source_root(dir.path(), Some(json!({ "reviews": [human("1.0.0")] })));
    let mut opts = options(&plugins, false);
    opts.ids = vec!["buz/a".into()];
    run(&opts).unwrap();

    let entries = read_ledger(&plugins);
    assert_eq!(entries.len(), 2, "the 1.1.0 pair is replaced, 1.0.0 kept");
    assert_eq!(entries[0]["version"], "1.1.0");
    assert_eq!(entries[0]["source"], "human");
    assert_eq!(entries[1]["version"], "1.0.0");
}

#[test]
fn unknown_id_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = source_root(dir.path(), None);
    let mut opts = options(&plugins, false);
    opts.ids = vec!["buz/missing".into()];
    let error = run(&opts).unwrap_err();
    assert!(format!("{error:#}").contains("not present in index.json"));
}

#[test]
fn mode_mismatches_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = source_root(dir.path(), None);
    let mut auto_with_ids = options(&plugins, true);
    auto_with_ids.ids = vec!["buz/a".into()];
    assert!(run(&auto_with_ids).is_err());

    let manual_without_ids = options(&plugins, false);
    assert!(run(&manual_without_ids).is_err());

    let mut manual_without_commit = options(&plugins, false);
    manual_without_commit.ids = vec!["buz/a".into()];
    manual_without_commit.commit = None;
    let error = run(&manual_without_commit).unwrap_err();
    assert!(format!("{error:#}").contains("--commit"));
}
