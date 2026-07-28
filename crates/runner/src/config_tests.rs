use super::*;

#[test]
fn accepts_runner_section() {
    let raw = r#"
[manager]
url = "http://127.0.0.1:8080"
register_token = "tok"

[runner]
name = "new-runner"
labels = ["docker"]
"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runner.toml");
    std::fs::write(&path, raw).unwrap();
    let cfg = load(Some(path.to_str().unwrap())).unwrap();
    assert_eq!(cfg.runner.name, "new-runner");
    assert_eq!(cfg.runner.labels, vec!["docker".to_string()]);
}
