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

fn read_ledger_text(plugins: &Path) -> String {
    std::fs::read_to_string(plugins.join("reviews.json")).unwrap()
}

fn rewrite_index(plugins: &Path, mutate: impl FnOnce(&mut Value)) {
    let path = plugins.join("index.json");
    let mut index: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    mutate(&mut index);
    std::fs::write(path, serde_json::to_string(&index).unwrap()).unwrap();
}

#[test]
fn auto_regeneration_is_byte_stable_when_content_did_not_change() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = source_root(dir.path(), Some(json!({ "reviews": [human("1.0.0")] })));
    run(&options(&plugins, true)).unwrap();
    let first = read_ledger_text(&plugins);

    let mut later = options(&plugins, true);
    later.date = Some("2026-12-31".into());
    later.commit = Some("fffffff".into());
    run(&later).unwrap();

    assert_eq!(
        read_ledger_text(&plugins),
        first,
        "an unchanged content hash must keep the entry verbatim — otherwise the \
         ledger drifts daily and every deploy attempts a write-back"
    );
}

#[test]
fn auto_restamps_only_the_version_whose_content_changed() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = source_root(dir.path(), Some(json!({ "reviews": [human("1.0.0")] })));
    run(&options(&plugins, true)).unwrap();

    rewrite_index(&plugins, |index| {
        for step in index["steps"].as_array_mut().unwrap() {
            if step["id"] == "buz/a" && step["version"] == "1.1.0" {
                step["sha256"] = json!("f".repeat(64));
            }
        }
    });
    let mut later = options(&plugins, true);
    later.date = Some("2026-11-11".into());
    later.commit = Some("def5678".into());
    run(&later).unwrap();

    let entries = read_ledger(&plugins);
    let find = |version: &str, source: &str| {
        entries
            .iter()
            .find(|entry| {
                entry["id"] == "buz/a" && entry["version"] == version && entry["source"] == source
            })
            .unwrap()
    };
    assert_eq!(find("1.0.0", "ci")["reviewed_at"], "2026-11-10");
    assert_eq!(find("1.0.0", "ci")["review_commit"], "abc1234");
    assert_eq!(find("1.1.0", "ci")["reviewed_at"], "2026-11-11");
    assert_eq!(find("1.1.0", "ci")["review_commit"], "def5678");
    assert_eq!(find("1.0.0", "human")["reviewed_at"], "2026-01-01");
}
