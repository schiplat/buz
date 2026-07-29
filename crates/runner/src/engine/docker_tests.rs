use super::{build_docker_run_args, preflight, resolve_host_work_dir, CONTAINER_WORKSPACE};
use std::collections::HashMap;
use std::path::PathBuf;

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

// resolve_host_work_dir now returns Result — security: rejects paths escaping runner work_dir.

#[test]
fn resolve_work_dir_joins_relative() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let base_str = base.to_str().unwrap();
    let p = resolve_host_work_dir(base_str, Some("./frontend")).unwrap();
    assert!(p.ends_with("frontend"), "{p:?}");
}

#[test]
fn resolve_work_dir_rejects_escape() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let base_str = base.to_str().unwrap();
    let err = resolve_host_work_dir(base_str, Some("/etc")).unwrap_err();
    assert!(err.to_string().contains("escapes"), "{err}");
}

#[test]
fn resolve_work_dir_defaults_to_runner() {
    let dir = tempfile::tempdir().unwrap();
    let expected = dir.path().canonicalize().unwrap();
    let base_str = expected.to_str().unwrap();
    let p = resolve_host_work_dir(base_str, None).unwrap();
    assert_eq!(p, expected);
}

#[test]
fn resolve_work_dir_allows_absolute_subpath() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(base.join("sub")).unwrap();
    let base_str = base.to_str().unwrap();
    let sub = base.join("sub");
    let p = resolve_host_work_dir(base_str, Some(sub.to_str().unwrap())).unwrap();
    assert!(
        p.ends_with("sub"),
        "expected to end with 'sub', got {p:?}"
    );
}

#[test]
fn build_args_includes_volumes() {
    let mut env = HashMap::new();
    env.insert("FOO".into(), "bar".into());
    let args = build_docker_run_args(
        "alpine:3.20",
        PathBuf::from("/tmp/ws").as_path(),
        "echo hi",
        &env,
        &[],
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
        PathBuf::from("/tmp/ws").as_path(),
        "hi",
        &HashMap::new(),
        &vols,
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
        PathBuf::from("/tmp/ws").as_path(),
        "hi",
        &HashMap::new(),
        &["".into(), "  ".into(), "  ".into()],
    );
    // Only the workspace mount should be present as -v
    let v_count = args.iter().filter(|a| a.as_str() == "-v").count();
    assert_eq!(v_count, 1, "only the workspace -v, not empty volumes");
}
