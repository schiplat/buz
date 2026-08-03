use super::*;

#[test]
fn accepts_flat_runner_section() {
    let raw = r#"
[runner]
name = "new-runner"
manager_url = "http://127.0.0.1:8080"
register_token = "tok"
labels = ["docker"]
workspace_dir = "/tmp/x"

[engine]
docker_enabled = true

[logging]
level = "debug"
"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runner.toml");
    std::fs::write(&path, raw).unwrap();
    let cfg = load(Some(path.to_str().unwrap())).unwrap();
    assert_eq!(cfg.runner.name, "new-runner");
    assert_eq!(cfg.runner.manager_url, "http://127.0.0.1:8080");
    assert_eq!(cfg.runner.register_token, "tok");
    assert_eq!(cfg.runner.workspace_dir, "/tmp/x");
    assert_eq!(cfg.runner.labels, vec!["docker".to_string()]);
    assert!(cfg.engine.docker_enabled);
    assert_eq!(cfg.logging.level, "debug");
}

#[test]
fn default_log_file_when_omitted() {
    let raw = r#"
[runner]
name = "r"
manager_url = "http://127.0.0.1:8080"
register_token = "tok"
"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runner.toml");
    std::fs::write(&path, raw).unwrap();
    let cfg = load(Some(path.to_str().unwrap())).unwrap();
    assert_eq!(cfg.logging.file.as_deref(), Some("./logs/runner.log"));
}

#[test]
fn empty_log_file_disables_default() {
    let raw = r#"
[runner]
name = "r"
manager_url = "http://127.0.0.1:8080"
register_token = "tok"

[logging]
file = ""
"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runner.toml");
    std::fs::write(&path, raw).unwrap();
    let cfg = load(Some(path.to_str().unwrap())).unwrap();
    assert!(cfg.logging.file.is_none());
}

#[test]
fn standalone_allows_empty_register_token() {
    let raw = r#"
[runner]
name = "standalone-runner"
manager_url = "http://127.0.0.1:8080"
standalone = true
"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runner.toml");
    std::fs::write(&path, raw).unwrap();
    let cfg = load(Some(path.to_str().unwrap())).unwrap();
    assert!(cfg.runner.standalone);
    assert!(cfg.runner.register_token.is_empty());
}

#[test]
fn rejects_empty_token_without_standalone() {
    let raw = r#"
[runner]
name = "r"
manager_url = "http://127.0.0.1:8080"
"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runner.toml");
    std::fs::write(&path, raw).unwrap();
    let err = load(Some(path.to_str().unwrap())).unwrap_err().to_string();
    assert!(err.contains("register_token is required"), "{err}");
}

#[test]
fn rejects_obsolete_manager_section() {
    let raw = r#"
[manager]
url = "http://127.0.0.1:8080"
register_token = "tok"

[runner]
name = "x"
"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runner.toml");
    std::fs::write(&path, raw).unwrap();
    let err = load(Some(path.to_str().unwrap())).unwrap_err().to_string();
    assert!(err.contains("obsolete [manager]"), "{err}");
}

#[test]
fn ensure_enabled_respects_engine_flags() {
    let mut eng = EngineConfig::default();
    assert!(eng.ensure_enabled("shell").is_ok());
    assert!(eng.ensure_enabled("local").is_ok());
    assert!(eng.ensure_enabled("ssh").is_ok());
    assert!(eng.ensure_enabled("docker").is_err());
    assert!(eng.ensure_enabled("wasm").is_err());

    eng.docker_enabled = true;
    assert!(eng.ensure_enabled("docker").is_ok());

    eng.shell_enabled = false;
    let err = eng.ensure_enabled("shell").unwrap_err().to_string();
    assert!(err.contains("shell_enabled"), "{err}");
}
