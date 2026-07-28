use super::{
    build_docker_run_args, preflight, resolve_host_work_dir, CONTAINER_WORKSPACE,
};
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

#[test]
fn resolve_work_dir_joins_relative() {
    let p = resolve_host_work_dir("/tmp/buz", Some("./frontend"));
    assert_eq!(p, PathBuf::from("/tmp/buz/frontend"));
}

#[test]
fn resolve_work_dir_keeps_absolute() {
    let p = resolve_host_work_dir("/tmp/buz", Some("/abs/ws"));
    assert_eq!(p, PathBuf::from("/abs/ws"));
}

#[test]
fn resolve_work_dir_defaults_to_runner() {
    let p = resolve_host_work_dir("/tmp/buz", None);
    assert_eq!(p, PathBuf::from("/tmp/buz"));
}

#[test]
fn build_args_includes_rm_volume_workdir_image_and_shell() {
    let mut env = HashMap::new();
    env.insert("FOO".into(), "bar".into());
    let args = build_docker_run_args(
        "alpine:3.20",
        PathBuf::from("/tmp/ws").as_path(),
        "echo hi",
        &env,
    );

    assert_eq!(args[0], "run");
    assert!(args.iter().any(|a| a == "--rm"));
    assert!(args.iter().any(|a| a == "-i"));
    let vol = format!("/tmp/ws:{CONTAINER_WORKSPACE}");
    assert!(args.iter().any(|a| a == &vol));
    assert!(args.windows(2).any(|w| w[0] == "-w" && w[1] == CONTAINER_WORKSPACE));
    assert!(args.iter().any(|a| a == "-e" ));
    assert!(args.iter().any(|a| a == "FOO=bar"));
    assert!(args.iter().any(|a| a == "INPUT_FOO=bar"));

    let img_pos = args.iter().position(|a| a == "alpine:3.20").expect("image");
    assert_eq!(args[img_pos + 1], "sh");
    assert_eq!(args[img_pos + 2], "-lc");
    assert_eq!(args[img_pos + 3], "echo hi");
}
