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

#[test]
fn ensure_enabled_respects_engine_flags() {
    let mut eng = EngineConfig::default();
    assert!(eng.ensure_enabled("shell").is_ok());
    assert!(eng.ensure_enabled("local").is_ok()); // backward compat
    assert!(eng.ensure_enabled("ssh").is_ok());
    assert!(eng.ensure_enabled("docker").is_err());
    assert!(eng.ensure_enabled("wasm").is_err());

    eng.docker_enabled = true;
    assert!(eng.ensure_enabled("docker").is_ok());

    eng.shell_enabled = false;
    let err = eng.ensure_enabled("shell").unwrap_err().to_string();
    assert!(err.contains("shell_enabled"), "{err}");
}
