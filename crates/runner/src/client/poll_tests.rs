use super::{
    build_login_banner, build_step_echo, build_step_failure_message, step_failure_hint,
    step_log_base, LOG_ECHO, LOG_LOGIN, LOG_STREAM_START, LOG_STEP_ERROR,
};
use std::collections::{HashMap, HashSet};

#[test]
fn step_log_base_isolates_steps() {
    assert_eq!(step_log_base(0), 0);
    assert_eq!(step_log_base(1), 10_000);
    assert_eq!(step_log_base(2), 20_000);
}

#[test]
fn login_echo_stream_do_not_collide() {
    let base = step_log_base(0);
    let login = base + LOG_LOGIN;
    let echo = base + LOG_ECHO;
    let stream0 = base + LOG_STREAM_START;
    let err = base + LOG_STEP_ERROR;
    assert_ne!(login, echo);
    assert_ne!(echo, stream0);
    assert_ne!(login, stream0);
    assert!(err > stream0);
}

#[test]
fn build_step_echo_uses_consistent_banner_and_fields() {
    let mut env = HashMap::new();
    env.insert("DEBIAN_FRONTEND".into(), "noninteractive".into());
    env.insert("acr_personal".into(), "secret-json".into());
    let mut secrets = HashSet::new();
    secrets.insert("acr_personal".into());

    let echo = build_step_echo(
        "docker",
        "echo hi\nuname -a || true\n",
        &env,
        &secrets,
        Some("/tmp/ws"),
    );

    assert!(echo.starts_with("==> RUN [docker]\n"), "{echo}");
    assert!(echo.contains("  note  GIT_TERMINAL_PROMPT=0"), "{echo}");
    assert!(echo.contains("  cwd   /tmp/ws\n"), "{echo}");
    assert!(echo.contains("acr_personal=***"), "{echo}");
    assert!(echo.contains("  cmd   echo hi\n"), "{echo}");
    assert!(echo.contains("        uname -a || true\n"), "{echo}");
}

#[test]
fn build_login_banner_matches_style() {
    assert_eq!(
        build_login_banner("registry.example.com"),
        "==> LOGIN [registry.example.com]\n"
    );
}

#[test]
fn step_failure_hint_detects_platform_mismatch() {
    let hint = step_failure_hint(
        "no matching manifest for linux/arm64/v8 in the manifest list",
        125,
    );
    assert!(hint.contains("architecture") || hint.contains("arm64"), "{hint}");
}

#[test]
fn step_failure_hint_detects_pull_denied() {
    let hint = step_failure_hint("pull access denied for example/repo", 125);
    assert!(hint.contains("registry"), "{hint}");
}

#[test]
fn build_step_failure_message_is_structured() {
    let msg = build_step_failure_message("Run private image", 125, "echo hi\nmore", "see above");
    assert!(msg.contains("failed (exit 125)"), "{msg}");
    assert!(msg.contains("  command  echo hi"), "{msg}");
    assert!(msg.contains("  hint     see above"), "{msg}");
}
