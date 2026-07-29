use super::{build_docker_run_args, preflight, CONTAINER_WORKSPACE};
use std::collections::HashMap;

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
    );

    assert_eq!(args[0], "run");
    assert!(args.iter().any(|a| a == "--rm"));
    assert!(args.iter().any(|a| a == "-i"));
    let vol = format!("/tmp/runner/wd:{CONTAINER_WORKSPACE}");
    assert!(args.iter().any(|a| a == &vol));
    assert!(args
        .windows(2)
        .any(|w| w[0] == "-w" && w[1] == CONTAINER_WORKSPACE));
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
    );

    let expected_wd = format!("{CONTAINER_WORKSPACE}/frontend");
    assert!(args
        .windows(2)
        .any(|w| w[0] == "-w" && w[1] == expected_wd));
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
    let args = build_docker_run_args(
        "alpine:3.20",
        "/tmp/ws",
        "echo hi",
        &env,
        &[],
        None,
    );

    assert_eq!(args[0], "run");
    assert!(args.iter().any(|a| a == "--rm"));
    assert!(args.iter().any(|a| a == "-i"));
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
    assert_eq!(args[img_pos + 2], "-lc");
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
    );

    // Search for the -v pairs — must appear before the image
    let img_pos = args.iter().position(|a| a == "alpine:3.20").expect("image");
    let vol_data_pos = args.iter().position(|a| a == "/data/app:/mnt/data").expect("vol1");
    let vol_cfg_pos = args.iter().position(|a| a == "/cfg:/etc/app").expect("vol2");
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
    );
    // Only the workspace mount should be present as -v
    let v_count = args.iter().filter(|a| a.as_str() == "-v").count();
    assert_eq!(v_count, 1, "only the workspace -v, not empty volumes");
}
