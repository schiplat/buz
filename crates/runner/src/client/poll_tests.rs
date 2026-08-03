use super::{
    build_config_prep_banner, build_login_banner, build_ssh_env_command, build_step_echo,
    build_step_failure_message, build_verbose_step_config, build_verbose_step_result,
    step_failure_hint, step_log_base, wrap_remote_cmd_with_registry_login, LOG_ECHO, LOG_LOGIN,
    LOG_STEP_ERROR, LOG_STEP_VERBOSE_CONFIG, LOG_STEP_VERBOSE_CONFIG_PREP, LOG_STEP_VERBOSE_EXEC,
    LOG_STEP_VERBOSE_PREPARE, LOG_STEP_VERBOSE_RESULT, LOG_STREAM_START,
};
use crate::masker::SecretCatalog;
use std::collections::HashMap;

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
fn verbose_chunk_order_matches_display_sequence() {
    // ORDER BY chunk_index must yield prepare → config → prep → login →
    // exec → run → stream → result → error.
    assert!(LOG_STEP_VERBOSE_PREPARE < LOG_STEP_VERBOSE_CONFIG);
    assert!(LOG_STEP_VERBOSE_CONFIG < LOG_STEP_VERBOSE_CONFIG_PREP);
    assert!(LOG_STEP_VERBOSE_CONFIG_PREP < LOG_LOGIN);
    assert!(LOG_LOGIN < LOG_STEP_VERBOSE_EXEC);
    assert!(LOG_STEP_VERBOSE_EXEC < LOG_ECHO);
    assert!(LOG_ECHO < LOG_STREAM_START);
    assert!(LOG_STREAM_START < LOG_STEP_VERBOSE_RESULT);
    assert!(LOG_STEP_VERBOSE_RESULT < LOG_STEP_ERROR);
}

#[test]
fn build_step_echo_masks_secret_and_masking_input_keys() {
    let mut env = HashMap::new();
    env.insert("DEBIAN_FRONTEND".into(), "noninteractive".into());
    env.insert("version".into(), "1.0".into());
    env.insert("deploy_token".into(), "tok-secret-value".into());
    env.insert("acr_personal".into(), "secret-json".into());
    let mut catalog = SecretCatalog::new();
    catalog.add_key("acr_personal");
    catalog.add_key("deploy_token");
    catalog.add_key("INPUT_DEPLOY_TOKEN");

    let echo = build_step_echo("shell", "echo hi\n", &env, &catalog, None);
    assert!(echo.contains("version=1.0"), "{echo}");
    assert!(echo.contains("deploy_token=***"), "{echo}");
    assert!(echo.contains("acr_personal=***"), "{echo}");
    assert!(!echo.contains("tok-secret-value"), "{echo}");
    assert!(!echo.contains("secret-json"), "{echo}");
}

#[test]
fn build_step_echo_uses_consistent_banner_and_fields() {
    let mut env = HashMap::new();
    env.insert("DEBIAN_FRONTEND".into(), "noninteractive".into());
    env.insert("SMOKE_ENV".into(), "from-pipeline".into());
    env.insert("acr_personal".into(), "secret-json".into());
    let mut catalog = SecretCatalog::new();
    catalog.add_key("acr_personal");

    let echo = build_step_echo(
        "docker",
        "echo hi\nuname -a || true\n",
        &env,
        &catalog,
        Some("/tmp/ws"),
    );

    assert!(echo.starts_with("==> RUN [docker]\n"), "{echo}");
    assert!(echo.contains("  note  GIT_TERMINAL_PROMPT=0"), "{echo}");
    assert!(echo.contains("  cwd   /tmp/ws\n"), "{echo}");
    assert!(echo.contains("SMOKE_ENV=from-pipeline"), "{echo}");
    assert!(echo.contains("acr_personal=***"), "{echo}");
    assert!(!echo.contains("secret-json"), "{echo}");
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
    assert!(
        hint.contains("architecture") || hint.contains("arm64"),
        "{hint}"
    );
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

#[test]
fn build_step_echo_shows_injected_hint_for_injected_keys() {
    let mut env = HashMap::new();
    env.insert(
        "BUZ_CONFIG_URL".into(),
        "http://oss.example/signed?sig=abc".into(),
    );
    env.insert("acr_personal".into(), "secret-json".into());
    let mut catalog = SecretCatalog::new();
    catalog.add_injected_key("BUZ_CONFIG_URL");
    catalog.add_key("acr_personal");

    let echo = build_step_echo("shell", "echo hi\n", &env, &catalog, None);
    assert!(echo.contains("BUZ_CONFIG_URL=<injected>"), "{echo}");
    assert!(echo.contains("acr_personal=***"), "{echo}");
    assert!(!echo.contains("http://oss.example"), "{echo}");
    assert!(!echo.contains("secret-json"), "{echo}");
}

#[test]
fn verbose_chunks_do_not_collide_with_meta_region() {
    let base = step_log_base(0);
    let mut set = std::collections::HashSet::new();
    for c in [
        base + LOG_STEP_VERBOSE_PREPARE,
        base + LOG_STEP_VERBOSE_CONFIG,
        base + LOG_STEP_VERBOSE_CONFIG_PREP,
        base + LOG_LOGIN,
        base + LOG_STEP_VERBOSE_EXEC,
        base + LOG_ECHO,
        base + LOG_STREAM_START,
        base + LOG_STEP_VERBOSE_RESULT,
        base + LOG_STEP_ERROR,
        base + 9998,
        base + 9999,
    ] {
        assert!(set.insert(c), "duplicate chunk index {c}");
    }
}

#[test]
fn verbose_step_config_renders_shape_without_secrets() {
    let step = serde_json::json!({
        "name": "deploy",
        "exec_type": "shell",
        "timeout_seconds": 3600,
        "fail_strategy": "stop",
        "work_dir": "/tmp/ws",
        "registry_secret": "acr_hk",
        "config_source": "super-manager",
        "config_key_path": "v1.2.3",
        "config_dest": "/opt/manager",
        "config_render": [{ "template": "env.example", "output": ".env" }],
        "hosts": ["h1", "h2"],
        "image": "",
    });
    let cfg = build_verbose_step_config(&step, "shell", "acr_hk");
    assert!(cfg.starts_with("==> STEP CONFIG\n"), "{cfg}");
    assert!(cfg.contains("  exec_type        shell"), "{cfg}");
    assert!(cfg.contains("  timeout          3600s"), "{cfg}");
    assert!(cfg.contains("  fail_strategy    stop"), "{cfg}");
    assert!(cfg.contains("  work_dir         /tmp/ws"), "{cfg}");
    assert!(cfg.contains("  registry_secret  acr_hk"), "{cfg}");
    assert!(
        cfg.contains("  config           super-manager (key: v1.2.3)"),
        "{cfg}"
    );
    assert!(cfg.contains("  config_dest      /opt/manager"), "{cfg}");
    assert!(
        cfg.contains("  config_render    env.example → .env"),
        "{cfg}"
    );
    assert!(cfg.contains("  hosts            h1, h2"), "{cfg}");
}

#[test]
fn config_prep_banner_redacts_url_unless_debug() {
    let step = serde_json::json!({
        "config_source": "OSS_DEMO",
        "config_dest": "/opt/manager",
        "config_auth": {
            "url": "https://bucket.oss.aliyuncs.com/config_tpl/demo/v1/configs.tar.gz?Signature=secret",
            "object_key": "config_tpl/demo/v1/configs.tar.gz",
            "expires_in_secs": 3600,
        },
    });
    let redacted = build_config_prep_banner(&step, false);
    assert!(redacted.starts_with("==> CONFIG PREP\n"), "{redacted}");
    assert!(
        redacted.contains("  source           OSS_DEMO"),
        "{redacted}"
    );
    assert!(
        redacted.contains("  dest             /opt/manager"),
        "{redacted}"
    );
    assert!(
        redacted.contains("  object_key       config_tpl/demo/v1/configs.tar.gz"),
        "{redacted}"
    );
    assert!(redacted.contains("  expires_in       3600s"), "{redacted}");
    assert!(redacted.contains("  url              ***"), "{redacted}");
    assert!(
        !redacted.contains("Signature=secret"),
        "verbose must not leak signed URL: {redacted}"
    );

    let debug = build_config_prep_banner(&step, true);
    assert!(
        debug.contains("Signature=secret"),
        "debug must show signed URL: {debug}"
    );
}

#[test]
fn config_prep_banner_render_only_mode() {
    let step = serde_json::json!({
        "config_dest": "/opt/manager",
        "config_render": [{ "template": "env.example", "output": ".env" }],
    });
    let banner = build_config_prep_banner(&step, false);
    assert!(
        banner.contains("  dest             /opt/manager"),
        "{banner}"
    );
    assert!(
        banner.contains("  mode             render-only"),
        "{banner}"
    );
    assert!(!banner.contains("object_key"), "{banner}");
    assert!(!banner.contains("url"), "{banner}");
}

#[test]
fn verbose_step_result_reports_duration_and_exit_code() {
    let r = build_verbose_step_result(std::time::Duration::from_millis(3210), 0);
    assert!(r.contains("==> STEP RESULT\n"), "{r}");
    assert!(r.contains("duration"), "{r}");
    assert!(r.contains("3.21s"), "{r}");
    assert!(r.contains("exit code  0"), "{r}");

    let short = build_verbose_step_result(std::time::Duration::from_millis(80), 3);
    assert!(short.contains("80ms"), "{short}");
    assert!(short.contains("exit code  3"), "{short}");
}

#[test]
fn ssh_env_command_prepends_set_e_and_exports_both_forms() {
    let mut env = HashMap::new();
    env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    env.insert("deploy_token".into(), "tok-secret-value".into());

    let cmd = build_ssh_env_command("echo hi\n", &env);
    assert!(cmd.starts_with("set -e; "), "{cmd}");
    assert!(cmd.contains("export GIT_TERMINAL_PROMPT='0'"), "{cmd}");
    assert!(
        cmd.contains("export INPUT_GIT_TERMINAL_PROMPT='0'"),
        "{cmd}"
    );
    assert!(
        cmd.contains("export deploy_token='tok-secret-value'"),
        "{cmd}"
    );
    assert!(
        cmd.contains("export INPUT_DEPLOY_TOKEN='tok-secret-value'"),
        "{cmd}"
    );
    assert!(cmd.ends_with("echo hi\n"), "{cmd}");
}

#[test]
fn ssh_env_command_empty_env_skips_exports() {
    let env = HashMap::new();
    let cmd = build_ssh_env_command("true\n", &env);
    assert_eq!(cmd, "set -e; true\n");
}

#[test]
fn registry_wrapper_keeps_fail_fast_via_exit_trap() {
    let wrapped = wrap_remote_cmd_with_registry_login("docker compose up -d\n");
    assert!(!wrapped.contains("set +e"), "{wrapped}");
    assert!(
        wrapped.contains("|| exit $?"),
        "login failure must exit before the run block: {wrapped}"
    );
    assert!(
        wrapped
            .contains("trap 'docker logout \"$BUZ_REGISTRY_HOST\" >/dev/null 2>&1 || true' EXIT"),
        "logout must be guaranteed via EXIT trap: {wrapped}"
    );
    assert!(wrapped.ends_with("docker compose up -d\n"), "{wrapped}");
}

#[test]
fn registry_wrapper_preserves_user_command_verbatim() {
    let wrapped = wrap_remote_cmd_with_registry_login(
        "set +e\nfalse\n_rc=$?\ndocker logout foo >/dev/null 2>&1 || true\nexit $_rc\n",
    );
    // A user block that opts out of fail-fast explicitly stays intact.
    assert!(
        wrapped.contains(
            "set +e\nfalse\n_rc=$?\ndocker logout foo >/dev/null 2>&1 || true\nexit $_rc\n"
        ),
        "{wrapped}"
    );
}

#[test]
fn full_ssh_registry_command_is_fail_fast_by_default() {
    let mut env = HashMap::new();
    env.insert("BUZ_REGISTRY_HOST".into(), "reg.example.com".into());
    env.insert("BUZ_REGISTRY_USER".into(), "u".into());
    env.insert("BUZ_REGISTRY_PASSWORD".into(), "p".into());
    let wrapped = wrap_remote_cmd_with_registry_login("docker compose up -d\n");
    let full = build_ssh_env_command(&wrapped, &env);

    assert!(full.starts_with("set -e; "), "{full}");
    assert!(!full.contains("set +e"), "{full}");
    assert!(full.contains("trap 'docker logout"), "{full}");
    assert!(full.contains("docker compose up -d\n"), "{full}");
}
