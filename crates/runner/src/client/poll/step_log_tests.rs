//! Tests for the step-log formatting layer (`super`): chunk layout
//! arithmetic, banner builders, and content redaction pins.

use super::*;
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
    const _: () = {
        assert!(LOG_STEP_VERBOSE_PREPARE < LOG_STEP_VERBOSE_CONFIG);
        assert!(LOG_STEP_VERBOSE_CONFIG < LOG_STEP_VERBOSE_CONFIG_PREP);
        assert!(LOG_STEP_VERBOSE_CONFIG_PREP < LOG_LOGIN);
        assert!(LOG_LOGIN < LOG_STEP_VERBOSE_EXEC);
        assert!(LOG_STEP_VERBOSE_EXEC < LOG_ECHO);
        assert!(LOG_ECHO < LOG_STREAM_START);
        assert!(LOG_STREAM_START < LOG_STEP_VERBOSE_RESULT);
        assert!(LOG_STEP_VERBOSE_RESULT < LOG_STEP_ERROR);
    };
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
fn runner_banner_identifies_version_and_os() {
    let banner = build_runner_banner();
    assert!(banner.starts_with("==> RUNNER\n"), "{banner}");
    assert!(
        banner.contains(&format!("  version          {}", shared::version::VERSION)),
        "{banner}"
    );
    // OS line is present and non-empty (never the "unknown" placeholder).
    assert!(banner.contains("  os               "), "{banner}");
    assert!(!banner.contains("  os               \n"), "{banner}");
}

#[test]
fn runner_banner_sorts_before_step_zero() {
    assert!(LOG_RUNNER_BANNER < step_log_base(0));
}

#[test]
fn build_ssh_banner_includes_target_and_workspace() {
    let banner = build_ssh_banner(&[
        ("deploy@10.0.0.1:22".into(), "/home/deploy".into()),
        ("deploy@10.0.0.2:2222".into(), String::new()),
    ]);
    assert!(
        banner.contains("==> SSH [deploy@10.0.0.1:22]\n"),
        "{banner}"
    );
    assert!(banner.contains("  workspace  /home/deploy\n"), "{banner}");
    assert!(
        banner.contains("==> SSH [deploy@10.0.0.2:2222]\n"),
        "{banner}"
    );
    assert!(
        !banner.contains("workspace  \n")
            || banner.lines().filter(|l| l.contains("10.0.0.2")).count() == 1,
        "empty workspace must omit workspace line: {banner}"
    );
    let only_second = banner
        .split("==> SSH [deploy@10.0.0.2:2222]\n")
        .nth(1)
        .unwrap();
    assert!(
        !only_second.contains("workspace"),
        "second host has no workspace: {banner}"
    );
}

#[test]
fn format_resolved_ssh_targets_from_host_objects() {
    let step = serde_json::json!({
        "host": [
            { "host": "a.example", "port": 22, "username": "ubuntu" },
            { "host": "b.example", "port": 2222, "username": "deploy" }
        ]
    });
    assert_eq!(
        format_resolved_ssh_targets(&step),
        vec!["ubuntu@a.example:22", "deploy@b.example:2222"]
    );
}

#[test]
fn verbose_step_config_prefers_resolved_host_objects() {
    let step = serde_json::json!({
        "exec_type": "ssh",
        "hosts": ["legacy-name"],
        "hosts_strategy": "chain",
        "host": {
            "host": "81.71.41.19",
            "port": 2424,
            "username": "root",
            "credential_value": "SECRET_KEY_MATERIAL"
        }
    });
    let cfg = build_verbose_step_config(&step, "ssh", "");
    assert!(
        cfg.contains("  hosts            root@81.71.41.19:2424"),
        "{cfg}"
    );
    assert!(cfg.contains("  hosts_strategy   chain"), "{cfg}");
    assert!(!cfg.contains("SECRET_KEY_MATERIAL"), "{cfg}");
    assert!(!cfg.contains("legacy-name"), "{cfg}");
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
        "config_source": "demo-app",
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
        cfg.contains("  config           demo-app (key: v1.2.3)"),
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
fn config_url_age_reads_the_signed_window() {
    let step = serde_json::json!({
        "config_auth": {
            "url": "https://bucket.oss.aliyuncs.com/a.tar.gz?Expires=1&Signature=s",
            "expires_in_secs": 3600,
            "signed_at_epoch": 1_000,
            "expires_epoch": 4_600,
        },
    });

    let age = config_url_age(&step, 1_102).expect("signed window present");
    assert_eq!(age.signed_at_epoch, 1_000);
    assert_eq!(age.expires_epoch, 4_600);
    assert_eq!(age.age_secs, 102);
    assert_eq!(age.remaining_secs, 3_498);
    assert_eq!(age.ttl_secs(), 3_600);
    assert!(!age.is_expired());
    assert_eq!(age.remaining_label(), "3498s");

    // Older Manager: no epoch fields → nothing to report.
    let legacy = serde_json::json!({
        "config_auth": { "url": "https://b/a.tar.gz", "expires_in_secs": 3600 },
    });
    assert!(config_url_age(&legacy, 1_102).is_none());
    assert!(config_url_age(&serde_json::json!({}), 1_102).is_none());

    // Past the deadline the label flips and the flag trips.
    let expired = config_url_age(&step, 4_661).expect("still readable");
    assert!(expired.is_expired());
    assert_eq!(expired.remaining_label(), "EXPIRED (61s ago)");
}

#[test]
fn config_prep_banner_reports_url_window_and_expiry() {
    let step = serde_json::json!({
        "config_source": "OSS_DEMO",
        "config_auth": {
            "url": "https://bucket.oss.aliyuncs.com/a.tar.gz?Signature=secret",
            "object_key": "config_tpl/demo/v1/configs.tar.gz",
            "expires_in_secs": 3600,
            "signed_at_epoch": chrono::Utc::now().timestamp() - 30,
            "expires_epoch": chrono::Utc::now().timestamp() + 3_570,
        },
    });
    let fresh = build_config_prep_banner(&step, false);
    assert!(fresh.contains("signed_at"), "{fresh}");
    assert!(fresh.contains("url_age"), "{fresh}");
    assert!(fresh.contains("url_remaining"), "{fresh}");
    assert!(
        fresh.contains(" UTC"),
        "signed_at must carry an absolute UTC stamp: {fresh}"
    );
    assert!(
        !fresh.contains("EXPIRED"),
        "a fresh URL must not be flagged: {fresh}"
    );
    assert!(!fresh.contains("url_warning"), "{fresh}");

    // Same step, but the Manager minted the URL an hour ago: already expired.
    let stale_step = serde_json::json!({
        "config_auth": {
            "url": "https://bucket.oss.aliyuncs.com/a.tar.gz?Signature=secret",
            "object_key": "config_tpl/demo/v1/configs.tar.gz",
            "expires_in_secs": 3600,
            "signed_at_epoch": chrono::Utc::now().timestamp() - 3_900,
            "expires_epoch": chrono::Utc::now().timestamp() - 300,
        },
    });
    let stale = build_config_prep_banner(&stale_step, false);
    assert!(
        stale.contains("url_remaining") && stale.contains("EXPIRED ("),
        "expired URL must be visible in the banner: {stale}"
    );
    assert!(stale.contains("url_warning"), "{stale}");
    assert!(stale.contains("already expired before download"), "{stale}");
    assert!(
        stale.contains("3600s TTL"),
        "warning should name the granted TTL: {stale}"
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
