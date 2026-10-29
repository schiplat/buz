use runner::config::{load, DockerConfigIsolation, DockerMountPolicy, EngineConfig};

fn write_config(dir: &tempfile::TempDir, raw: &str) -> String {
    let path = dir.path().join("runner.toml");
    std::fs::write(&path, raw).unwrap();
    path.to_str().unwrap().to_string()
}

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
    let cfg = load(Some(&write_config(&dir, raw))).unwrap();
    assert_eq!(cfg.runner.name, "new-runner");
    assert_eq!(cfg.runner.manager_url, "http://127.0.0.1:8080");
    assert_eq!(cfg.runner.register_token, "tok");
    assert_eq!(cfg.runner.workspace_dir, "/tmp/x");
    assert_eq!(cfg.runner.docker_mount_policy, DockerMountPolicy::Legacy);
    assert!(!cfg.runner.docker_socket_allowed);
    assert_eq!(cfg.runner.docker_socket_path, "/var/run/docker.sock");
    assert_eq!(cfg.runner.token_cache_path, "./state/runner-token");
    assert_eq!(cfg.runner.labels, vec!["docker".to_string()]);
    assert!(cfg.engine.docker_enabled);
    assert_eq!(cfg.logging.level, "debug");
}

#[test]
fn token_cache_path_can_be_configured() {
    let raw = r#"
[runner]
name = "r"
manager_url = "http://127.0.0.1:8080"
register_token = "tok"
token_cache_path = "/var/lib/buz/runner-token"
"#;
    let dir = tempfile::tempdir().unwrap();
    let cfg = load(Some(&write_config(&dir, raw))).unwrap();
    assert_eq!(cfg.runner.token_cache_path, "/var/lib/buz/runner-token");
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
    let cfg = load(Some(&write_config(&dir, raw))).unwrap();
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
    let cfg = load(Some(&write_config(&dir, raw))).unwrap();
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
    let cfg = load(Some(&write_config(&dir, raw))).unwrap();
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
    let err = load(Some(&write_config(&dir, raw)))
        .unwrap_err()
        .to_string();
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
    let err = load(Some(&write_config(&dir, raw)))
        .unwrap_err()
        .to_string();
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

#[test]
fn docker_mount_policy_and_socket_authorization_are_machine_local() {
    let raw = r#"
[runner]
name = "r"
register_token = "tok"
docker_mount_policy = "strict"
docker_socket_allowed = true
docker_socket_path = "/run/docker/daemon.sock"
"#;
    let dir = tempfile::tempdir().unwrap();
    let cfg = load(Some(&write_config(&dir, raw))).unwrap();
    assert_eq!(cfg.runner.docker_mount_policy, DockerMountPolicy::Strict);
    assert!(cfg.runner.docker_socket_allowed);
    assert_eq!(cfg.runner.docker_socket_path, "/run/docker/daemon.sock");
}

#[test]
fn docker_mount_policy_rejects_unknown_mode() {
    let raw =
        "[runner]\\nregister_token = \\\"tok\\\"\\ndocker_mount_policy = \\\"permissive\\\"\\n";
    let dir = tempfile::tempdir().unwrap();
    assert!(load(Some(&write_config(&dir, raw))).is_err());
}

#[test]
fn docker_config_isolation_defaults_to_per_job() {
    let raw = r#"
[runner]
name = "r"
register_token = "tok"

[engine]
docker_enabled = true
"#;
    let dir = tempfile::tempdir().unwrap();
    let cfg = load(Some(&write_config(&dir, raw))).unwrap();
    assert_eq!(
        cfg.engine.docker_config_isolation,
        DockerConfigIsolation::PerJob
    );
}

#[test]
fn docker_config_isolation_shared_is_opt_in() {
    let raw = r#"
[runner]
name = "r"
register_token = "tok"

[engine]
docker_config_isolation = "shared"
"#;
    let dir = tempfile::tempdir().unwrap();
    let cfg = load(Some(&write_config(&dir, raw))).unwrap();
    assert_eq!(
        cfg.engine.docker_config_isolation,
        DockerConfigIsolation::Shared
    );
}

#[test]
fn docker_config_isolation_rejects_unknown_value() {
    let raw = r#"
[runner]
name = "r"
register_token = "tok"

[engine]
docker_config_isolation = "global"
"#;
    let dir = tempfile::tempdir().unwrap();
    assert!(load(Some(&write_config(&dir, raw))).is_err());
}

#[test]
fn defaults_are_host_mode_with_local_sandbox_wiring_only() {
    let raw = r#"
[runner]
name = "r"
register_token = "tok"
"#;
    let dir = tempfile::tempdir().unwrap();
    let cfg = load(Some(&write_config(&dir, raw))).unwrap();
    assert_eq!(
        cfg.execution_mode,
        shared::protocol::RunnerExecutionMode::Host
    );
    assert_eq!(
        cfg.sandbox.supervisor_socket,
        "/run/buz-sandboxd/sandboxd.sock"
    );
    assert_eq!(cfg.sandbox.snapshot_path, "./state/sandboxd.snapshot.json");
    assert_eq!(cfg.sandbox.request_timeout_seconds, 30);
}

#[test]
fn sandbox_section_keeps_only_host_local_wiring_and_ignores_legacy_tier_keys() {
    // Tier images/runtimes are Manager-managed now; a runner.toml that still
    // carries them must load (ignored), not fail, so operators can upgrade
    // binaries before cleaning configs.
    let raw = r#"
[runner]
name = "r"
register_token = "tok"

[sandbox]
supervisor_socket = "/tmp/sbx.sock"
workspace_root = "/srv/ws"
snapshot_path = "/var/lib/buz/sandboxd.snapshot.json"
container_image = "reg/job@sha256:aaa"
sandbox_runtime = "runsc"
firecracker_guest_image = "reg/vm@sha256:ccc"
"#;
    let dir = tempfile::tempdir().unwrap();
    let cfg = load(Some(&write_config(&dir, raw))).unwrap();
    assert_eq!(cfg.sandbox.supervisor_socket, "/tmp/sbx.sock");
    assert_eq!(cfg.sandbox.workspace_root, "/srv/ws");
    assert_eq!(
        cfg.sandbox.snapshot_path,
        "/var/lib/buz/sandboxd.snapshot.json"
    );
}
