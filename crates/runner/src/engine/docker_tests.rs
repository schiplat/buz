use super::{
    build_docker_run_args, build_docker_run_args_with_socket, preflight,
    pull_image_with_program_and_timeout, validate_step_policy, CONTAINER_WORKSPACE,
};
use crate::config::{DockerMountPolicy, RunnerSection};
use std::collections::HashMap;
use std::time::Duration;

#[cfg(unix)]
fn temp_socket() -> (
    tempfile::TempDir,
    std::os::unix::net::UnixListener,
    std::path::PathBuf,
) {
    use std::os::unix::net::UnixListener;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("docker.sock");
    let listener = UnixListener::bind(&path).unwrap();
    (dir, listener, path)
}

#[test]
fn strict_policy_allows_workspace_without_extra_capabilities() {
    let mut runner = RunnerSection::default();
    runner.docker_mount_policy = DockerMountPolicy::Strict;
    assert_eq!(
        validate_step_policy(&runner, &[], &[], false, false).unwrap(),
        None
    );
}

#[test]
fn strict_policy_rejects_explicit_mount_privileged_and_host_prepare() {
    let mut runner = RunnerSection::default();
    runner.docker_mount_policy = DockerMountPolicy::Strict;
    assert!(validate_step_policy(&runner, &[], &["/data:/data".into()], false, false).is_err());
    assert!(validate_step_policy(&runner, &[], &[], true, false).is_err());
    assert!(validate_step_policy(&runner, &[], &[], false, true).is_err());
}

#[test]
fn socket_capability_requires_local_allow_and_valid_source() {
    let mut runner = RunnerSection::default();
    runner.docker_mount_policy = DockerMountPolicy::Strict;
    let (socket_dir, _listener, socket_path) = temp_socket();
    runner.docker_socket_path = socket_path.to_string_lossy().into_owned();
    let cap = vec!["docker-socket".to_string()];
    assert!(validate_step_policy(&runner, &cap, &[], false, false).is_err());
    runner.docker_socket_allowed = true;
    let authorized_path = validate_step_policy(&runner, &cap, &[], false, false).unwrap();
    assert_eq!(
        authorized_path.as_deref(),
        Some(socket_path.canonicalize().unwrap().as_path())
    );
    assert!(socket_dir.path().exists());
    runner.docker_socket_path = "/run/../docker.sock".into();
    assert!(validate_step_policy(&runner, &cap, &[], false, false).is_err());
}

#[test]
fn strict_policy_accepts_capability_only_and_rejects_explicit_socket_mounts() {
    let mut runner = RunnerSection::default();
    runner.docker_mount_policy = DockerMountPolicy::Strict;
    let (socket_dir, _listener, socket_path) = temp_socket();
    runner.docker_socket_allowed = true;
    runner.docker_socket_path = socket_path.to_string_lossy().into_owned();
    let cap = vec!["docker-socket".to_string()];
    assert!(validate_step_policy(&runner, &cap, &[], false, false)
        .unwrap()
        .is_some());
    let duplicate = vec![
        format!("{}:/var/run/docker.sock", socket_path.display()),
        format!("{}:/var/run/docker.sock", socket_path.display()),
    ];
    assert!(validate_step_policy(&runner, &cap, &duplicate, false, false).is_err());
    let alias = socket_path.with_extension("sock-link");
    std::os::unix::fs::symlink(&socket_path, &alias).unwrap();
    runner.docker_socket_path = alias.to_string_lossy().into_owned();
    assert!(validate_step_policy(&runner, &cap, &[], false, false).is_err());
    assert!(socket_dir.path().exists());
}

#[test]
fn legacy_policy_keeps_normal_mounts_but_socket_requires_local_authorization() {
    let mut runner = RunnerSection::default();
    assert!(
        validate_step_policy(&runner, &[], &["/data:/data".into()], false, false)
            .unwrap()
            .is_none()
    );
    let (socket_dir, _listener, socket_path) = temp_socket();
    runner.docker_socket_path = socket_path.to_string_lossy().into_owned();
    let socket_volume = format!("{}:/var/run/docker.sock", socket_path.display());
    assert!(validate_step_policy(&runner, &[], &[socket_volume.clone()], false, false).is_err());
    runner.docker_socket_allowed = true;
    assert!(
        validate_step_policy(&runner, &[], &[socket_volume], false, false)
            .unwrap()
            .is_some()
    );
    assert!(validate_step_policy(
        &runner,
        &[],
        &[format!("{}:/host", socket_path.display())],
        false,
        false
    )
    .is_err());
    assert!(socket_dir.path().exists());
}

#[test]
fn capabilities_reject_unknown_duplicates_and_non_docker() {
    let runner = RunnerSection::default();
    assert!(validate_step_policy(&runner, &["host-root".into()], &[], false, false).is_err());
    assert!(validate_step_policy(
        &runner,
        &["docker-socket".into(), "docker-socket".into()],
        &[],
        false,
        false
    )
    .is_err());
}

#[test]
fn socket_mount_is_added_once_and_remote_docker_env_is_not_forwarded() {
    let mut env = HashMap::new();
    env.insert("DOCKER_HOST".into(), "tcp://attacker:2375".into());
    env.insert("DOCKER_CONTEXT".into(), "remote".into());
    env.insert("NORMAL".into(), "yes".into());
    let (socket_dir, _listener, socket_path) = temp_socket();
    let args = build_docker_run_args_with_socket(
        "alpine",
        "/tmp/ws",
        "true",
        &env,
        &[],
        None,
        false,
        Some(socket_path.as_path()),
        None,
    );
    assert_eq!(args.iter().filter(|a| a.as_str() == "-v").count(), 2);
    assert!(!args.iter().any(|a| a.starts_with("DOCKER_HOST=")));
    assert!(!args.iter().any(|a| a.starts_with("DOCKER_CONTEXT=")));
    assert!(args.iter().any(|a| a == "NORMAL=yes"));
    assert!(socket_dir.path().exists());
}

#[test]
fn preflight_rejects_disabled() {
    let err = preflight(false, "alpine:3.20").unwrap_err().to_string();
    assert!(err.contains("disabled"), "{err}");
    assert!(err.contains("docker_enabled"), "{err}");
}

#[test]
fn preflight_rejects_empty_image() {
    let err = preflight(true, "  ").unwrap_err().to_string();
    assert!(err.contains("image"), "{err}");
}

#[test]
fn preflight_ok() {
    assert!(preflight(true, "alpine:3.20").is_ok());
}

// ── build_docker_run_args (new GitHub Actions style) ──

#[test]
fn build_args_mounts_workspace_and_default_cwd() {
    let args = build_docker_run_args(
        "alpine:3.20",
        "/tmp/runner/wd",
        "echo hi",
        &HashMap::new(),
        &[],
        None,
        false,
    );

    assert_eq!(args[0], "run");
    assert!(args.iter().any(|a| a == "--rm"));
    assert!(
        !args.iter().any(|a| a == "-i"),
        "-i (interactive) should not be used in CI"
    );
    let vol = format!("/tmp/runner/wd:{CONTAINER_WORKSPACE}");
    assert!(args.iter().any(|a| a == &vol));
    assert!(args
        .windows(2)
        .any(|w| w[0] == "-w" && w[1] == CONTAINER_WORKSPACE));
}

#[test]
fn build_args_privileged_flag() {
    let args = build_docker_run_args(
        "docker:27-dind",
        "/tmp/ws",
        "echo hi",
        &HashMap::new(),
        &[],
        None,
        true,
    );
    assert!(
        args.iter().any(|a| a == "--privileged"),
        "privileged flag present"
    );
}

#[test]
fn build_args_custom_container_work_dir() {
    let args = build_docker_run_args(
        "alpine:3.20",
        "/tmp/runner/wd",
        "pwd",
        &HashMap::new(),
        &[],
        Some("frontend"),
        false,
    );

    let expected_wd = format!("{CONTAINER_WORKSPACE}/frontend");
    assert!(args.windows(2).any(|w| w[0] == "-w" && w[1] == expected_wd));
}

#[test]
fn build_args_empty_container_work_dir_ignored() {
    let args = build_docker_run_args(
        "alpine:3.20",
        "/tmp/runner/wd",
        "pwd",
        &HashMap::new(),
        &[],
        Some("  "),
        false,
    );

    // Whitespace-only → falls back to workspace root
    assert!(args
        .windows(2)
        .any(|w| w[0] == "-w" && w[1] == CONTAINER_WORKSPACE));
}

#[test]
fn build_args_includes_volumes() {
    let mut env = HashMap::new();
    env.insert("FOO".into(), "bar".into());
    let args = build_docker_run_args("alpine:3.20", "/tmp/ws", "echo hi", &env, &[], None, false);

    assert_eq!(args[0], "run");
    assert!(args.iter().any(|a| a == "--rm"));
    assert!(
        !args.iter().any(|a| a == "-i"),
        "-i (interactive) should not be used in CI"
    );
    let vol = format!("/tmp/ws:{CONTAINER_WORKSPACE}");
    assert!(args.iter().any(|a| a == &vol));
    assert!(args
        .windows(2)
        .any(|w| w[0] == "-w" && w[1] == CONTAINER_WORKSPACE));
    assert!(args.iter().any(|a| a == "-e"));
    assert!(args.iter().any(|a| a == "FOO=bar"));
    assert!(args.iter().any(|a| a == "INPUT_FOO=bar"));

    let img_pos = args.iter().position(|a| a == "alpine:3.20").expect("image");
    assert_eq!(args[img_pos + 1], "sh");
    assert_eq!(args[img_pos + 2], "-elc");
    assert_eq!(args[img_pos + 3], "echo hi");
}

#[test]
fn build_args_extra_volumes() {
    let vols = vec!["/data/app:/mnt/data".into(), "/cfg:/etc/app".into()];
    let args = build_docker_run_args(
        "alpine:3.20",
        "/tmp/ws",
        "hi",
        &HashMap::new(),
        &vols,
        None,
        false,
    );

    // Search for the -v pairs — must appear before the image
    let img_pos = args.iter().position(|a| a == "alpine:3.20").expect("image");
    let vol_data_pos = args
        .iter()
        .position(|a| a == "/data/app:/mnt/data")
        .expect("vol1");
    let vol_cfg_pos = args
        .iter()
        .position(|a| a == "/cfg:/etc/app")
        .expect("vol2");
    assert!(vol_data_pos < img_pos, "vol1 before image");
    assert!(vol_cfg_pos < img_pos, "vol2 before image");
    // preceding arg must be -v
    assert_eq!(args[vol_data_pos - 1], "-v");
    assert_eq!(args[vol_cfg_pos - 1], "-v");
}

#[test]
fn build_args_empty_volumes_ignored() {
    let args = build_docker_run_args(
        "alpine:3.20",
        "/tmp/ws",
        "hi",
        &HashMap::new(),
        &["".into(), "  ".into(), "  ".into()],
        None,
        false,
    );
    // Only the workspace mount should be present as -v
    let v_count = args.iter().filter(|a| a.as_str() == "-v").count();
    assert_eq!(v_count, 1, "only the workspace -v, not empty volumes");
}

#[tokio::test]
async fn registry_login_rejects_empty_fields() {
    let err = super::registry_login("", "u", "p", None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("registry_auth"), "{err}");
    let err = super::registry_login("reg.example", "", "p", None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("registry_auth"), "{err}");
    let err = super::registry_login("reg.example", "u", "", None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("registry_auth"), "{err}");
}

#[cfg(unix)]
#[tokio::test]
async fn pull_image_uses_config_and_image_argument() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let docker = dir.path().join("docker");
    let output = dir.path().join("args");
    std::fs::write(
        &docker,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
            output.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = dir.path().join("anonymous-config");
    std::fs::create_dir(&config).unwrap();

    pull_image_with_program_and_timeout(
        &docker,
        "alpine@sha256:deadbeef",
        Some(config.to_str().unwrap()),
        Duration::from_secs(1),
    )
    .await
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        "pull\nalpine@sha256:deadbeef\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_pull_future_kills_child() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let docker = dir.path().join("docker");
    let pid_file = dir.path().join("pid");
    std::fs::write(
        &docker,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n",
            pid_file.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o700)).unwrap();

    let pull = tokio::spawn(async move {
        pull_image_with_program_and_timeout(
            &docker,
            "alpine@sha256:deadbeef",
            None,
            Duration::from_secs(30),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fake docker process should start");
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    pull.abort();
    let _ = pull.await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let result = std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .unwrap();
    assert!(!result.success(), "pull child process should be terminated");
}
