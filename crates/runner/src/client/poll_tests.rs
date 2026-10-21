use super::{
    build_config_prep_banner, build_login_banner, build_runner_banner, build_ssh_banner,
    build_ssh_env_command, build_step_echo, build_step_failure_message, build_verbose_step_config,
    build_verbose_step_result, buz_metadata_env, clean_job_docker_config, config_url_age,
    format_resolved_ssh_targets, job_docker_config_dir, merge_ssh_host_outputs, parse_buz_env_file,
    prepare_job_docker_config_dir, step_failure_hint, step_log_base,
    wrap_remote_cmd_with_registry_login, LOG_ECHO, LOG_LOGIN, LOG_RUNNER_BANNER, LOG_STEP_ERROR,
    LOG_STEP_VERBOSE_CONFIG, LOG_STEP_VERBOSE_CONFIG_PREP, LOG_STEP_VERBOSE_EXEC,
    LOG_STEP_VERBOSE_PREPARE, LOG_STEP_VERBOSE_RESULT, LOG_STREAM_START,
};
use crate::masker::SecretCatalog;
use std::collections::HashMap;

#[test]
fn work_dir_rejects_absolute_parent_and_prefix_escape() {
    assert!(!super::is_safe_workspace_subdir("/tmp"));
    assert!(!super::is_safe_workspace_subdir("../outside"));
    assert!(!super::is_safe_workspace_subdir("src/../../outside"));
    assert!(!super::is_safe_workspace_subdir(""));
    assert!(super::is_safe_workspace_subdir("frontend/src"));
}

#[test]
fn buz_env_file_parses_kv_heredoc_and_last_write_wins() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("env");
    std::fs::write(
        &path,
        "# comment\n\
         APP_VERSION=1.2.3\n\
         MULTI<<EOF\nline one\nline two\nEOF\n\
         EMPTY=\n\
         APP_VERSION=9.9.9\n\
         BAD LINE WITHOUT EQUALS\n\
         1BAD=skip\n",
    )
    .unwrap();
    let got = parse_buz_env_file(path.to_str().unwrap()).unwrap();
    assert_eq!(
        got,
        vec![
            ("MULTI".to_string(), "line one\nline two".to_string()),
            ("EMPTY".to_string(), String::new()),
            ("APP_VERSION".to_string(), "9.9.9".to_string()),
        ]
    );
}

#[test]
fn buz_env_file_missing_is_empty_and_unterminated_block_dropped() {
    assert!(parse_buz_env_file("/nonexistent/buz-env")
        .unwrap()
        .is_empty());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("env");
    std::fs::write(&path, "OK=yes\nBROKEN<<EOF\nnever closed\n").unwrap();
    let got = parse_buz_env_file(path.to_str().unwrap()).unwrap();
    assert_eq!(got, vec![("OK".to_string(), "yes".to_string())]);
}

#[test]
fn expand_env_refs_resolves_and_fails_closed() {
    let mut values = HashMap::new();
    values.insert("TAG".to_string(), "1.0".to_string());
    let malformed = shared::secret_ref::expand_env_refs("app:${env.}", &values).unwrap_err();
    assert!(malformed.contains("invalid"), "{malformed}");

    // `${TAG}` is not an env ref — left verbatim; only ${env.*} expands.
    let ok = shared::secret_ref::expand_env_refs("app:${TAG}-${env.TAG}", &values).unwrap();
    assert_eq!(ok, "app:${TAG}-1.0");
    let missing = shared::secret_ref::expand_env_refs("${env.MISSING}", &values).unwrap_err();
    assert!(missing.contains("MISSING"), "{missing}");
}

#[test]
fn ssh_env_command_drops_buz_env_path() {
    let mut env = HashMap::new();
    env.insert("BUZ_ENV".into(), "/tmp/buz-runner/job/.buz/env".into());
    env.insert("APP_VERSION".into(), "1.2.3".into());
    let cmd = build_ssh_env_command("deploy", &env);
    assert!(!cmd.contains("BUZ_ENV="), "{cmd}");
    // Dynamic values still reach the remote host as ordinary vars.
    assert!(cmd.contains("export APP_VERSION='1.2.3'"), "{cmd}");
    assert!(cmd.contains("export INPUT_APP_VERSION='1.2.3'"), "{cmd}");
}

#[test]
fn docker_policy_error_preflight_test_anchor() {
    let mut runner = crate::config::RunnerSection::default();
    runner.docker_mount_policy = crate::config::DockerMountPolicy::Strict;
    assert!(crate::engine::docker::validate_step_policy(&runner, &[], &[], false, true).is_err());
}

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
fn merge_ssh_host_outputs_any_failure_fails_step() {
    let out = merge_ssh_host_outputs(
        "Deploy",
        true,
        vec![
            (
                "host-b".into(),
                Ok(crate::engine::ssh::SshOutput {
                    stdout: "ok-b\n".into(),
                    stderr: String::new(),
                    exit_code: 0,
                }),
            ),
            (
                "host-a".into(),
                Ok(crate::engine::ssh::SshOutput {
                    stdout: "fail-a\n".into(),
                    stderr: "boom\n".into(),
                    exit_code: 7,
                }),
            ),
        ],
    );
    assert_eq!(out.exit_code, 7);
    assert!(
        out.stdout.contains("--- Deploy @ host-a ---"),
        "{:?}",
        out.stdout
    );
    assert!(
        out.stdout.contains("--- Deploy @ host-b ---"),
        "{:?}",
        out.stdout
    );
    // Sorted by host label: a before b
    let pos_a = out.stdout.find("host-a").unwrap();
    let pos_b = out.stdout.find("host-b").unwrap();
    assert!(pos_a < pos_b, "hosts sorted: {}", out.stdout);
    assert!(out.stdout.contains("--- summary ---"), "{}", out.stdout);
    assert!(out.stdout.contains("host-a: exit 7"), "{}", out.stdout);
    assert!(out.stdout.contains("host-b: exit 0"), "{}", out.stdout);
}

#[test]
fn merge_ssh_host_outputs_error_sets_minus_one() {
    let out = merge_ssh_host_outputs(
        "x",
        false,
        vec![("only".into(), Err(anyhow::anyhow!("channel died")))],
    );
    assert_eq!(out.exit_code, -1);
    assert!(out.stderr.contains("ERROR: channel died"), "{}", out.stderr);
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

#[test]
fn buz_metadata_env_normalizes_claim_metadata() {
    let task = serde_json::json!({
        "metadata": {
            "pipeline_name": "deploy",
            "team": "platform",
            "trigger": "webhook",
            "triggered_by": "alice"
        },
        "inputs": { "GIT_SHA": "abc1234", "GIT_BRANCH": "main" }
    });
    let env = buz_metadata_env(&task, "job-1", "http://m:8080/");
    assert!(env.contains(&("BUZ_JOB_ID".into(), "job-1".into())));
    assert!(env.contains(&("BUZ_JOB_URL".into(), "http://m:8080/jobs/job-1".into())));
    assert!(env.contains(&("BUZ_PIPELINE_NAME".into(), "deploy".into())));
    assert!(env.contains(&("BUZ_TEAM".into(), "platform".into())));
    assert!(env.contains(&("BUZ_TRIGGER".into(), "webhook".into())));
    assert!(env.contains(&("BUZ_TRIGGERED_BY".into(), "alice".into())));
    assert!(env.contains(&("BUZ_GIT_SHA".into(), "abc1234".into())));
    assert!(env.contains(&("BUZ_GIT_BRANCH".into(), "main".into())));
    assert!(env.iter().all(|(k, _)| k != "BUZ_GIT_TAG"), "{env:?}");
}

#[test]
fn buz_metadata_env_carries_run_numbers_when_present() {
    let task = serde_json::json!({
        "metadata": {
            "pipeline_name": "deploy",
            "run_number": 42,
            "run_attempt": 1
        }
    });
    let env = buz_metadata_env(&task, "job-9", "http://m:8080");
    assert!(
        env.contains(&("BUZ_RUN_NUMBER".into(), "42".into())),
        "{env:?}"
    );
    assert!(
        env.contains(&("BUZ_RUN_ATTEMPT".into(), "1".into())),
        "{env:?}"
    );
}

#[test]
fn buz_metadata_env_omits_run_numbers_for_legacy_claims() {
    let env = buz_metadata_env(
        &serde_json::json!({"metadata": {"pipeline_name": "p"}}),
        "j",
        "http://m",
    );
    assert!(env.iter().all(|(k, _)| k != "BUZ_RUN_NUMBER"), "{env:?}");
    assert!(env.iter().all(|(k, _)| k != "BUZ_RUN_ATTEMPT"), "{env:?}");
}

#[test]
fn buz_metadata_env_omits_missing_fields() {
    let env = buz_metadata_env(&serde_json::json!({}), "job-2", "http://m:8080");
    assert_eq!(
        env,
        vec![
            ("BUZ_JOB_ID".to_string(), "job-2".to_string()),
            (
                "BUZ_JOB_URL".to_string(),
                "http://m:8080/jobs/job-2".to_string()
            ),
        ]
    );
}

#[tokio::test]
async fn marketplace_input_validation_error_logs_and_fails_job_before_execution() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut buf = [0_u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "connection closed before complete HTTP headers");
            request.extend_from_slice(&buf[..n]);
            if let Some(pos) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        while request.len() < header_end + content_length {
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "connection closed before complete HTTP body");
            request.extend_from_slice(&buf[..n]);
        }
        String::from_utf8_lossy(&request).into_owned()
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().await.unwrap();
            requests.push(read_http_request(&mut stream).await);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        }
        requests
    });

    let client = reqwest::Client::new();
    let config = crate::config::Config {
        runner: crate::config::RunnerSection {
            manager_url: format!("http://{addr}"),
            ..Default::default()
        },
        ..Default::default()
    };
    let task = serde_json::json!({
        "job_id": "job-marketplace-invalid",
        "tasks": [
            {
                "name": "invalid-template-input",
                "command_template": "echo must-not-run",
                "exec_type": "shell",
                "input_validation_error": "inputs.webhook: does not match pattern",
                "step_env": { "INPUT_WEBHOOK": "not-a-url" },
                "masking_env_keys": ["INPUT_WEBHOOK"]
            },
            {
                "name": "must-not-run-after-validation-failure",
                "command_template": "echo must-also-not-run",
                "exec_type": "shell"
            }
        ]
    });

    let (_cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    let error = super::execute_job(&client, &config, "runner-token", &task, &mut cancel_rx)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("marketplace input validation"));

    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].starts_with("POST /api/v1/jobs/job-marketplace-invalid/logs "));
    assert!(requests[1].starts_with("PATCH /api/v1/jobs/job-marketplace-invalid/status "));
    assert!(requests[1].contains("\"status\":\"running\""));
    assert!(requests[2].starts_with("POST /api/v1/jobs/job-marketplace-invalid/logs "));
    assert!(requests[2].contains("marketplace input validation"));
    assert!(requests[2].contains("does not match pattern"));
    assert!(requests[3].starts_with("PATCH /api/v1/jobs/job-marketplace-invalid/status "));
    assert!(requests[3].contains("\"status\":\"failed\""));
    assert!(requests
        .iter()
        .all(|request| { !request.contains("must-not-run") && !request.contains("not-a-url") }));
}

#[test]
fn job_docker_config_dir_is_workspace_scoped() {
    assert_eq!(
        job_docker_config_dir("/tmp/buz-runner/job-1"),
        "/tmp/buz-runner/job-1/.docker"
    );
    assert_eq!(
        job_docker_config_dir("/tmp/buz-runner/job-1/"),
        "/tmp/buz-runner/job-1/.docker"
    );
}

#[cfg(unix)]
#[test]
fn prepare_creates_private_dir_for_per_job_isolation() {
    use crate::config::DockerConfigIsolation;

    let dir = tempfile::tempdir().unwrap();
    let job_ws = dir.path().join("job-1");
    std::fs::create_dir_all(&job_ws).unwrap();

    prepare_job_docker_config_dir(job_ws.to_str().unwrap(), DockerConfigIsolation::PerJob).unwrap();
    let docker_dir = job_ws.join(".docker");
    assert!(docker_dir.is_dir());
    use std::os::unix::fs::PermissionsExt;
    let mode = docker_dir.metadata().unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700, "docker config dir must be 0700");

    // Shared mode: no directory is created.
    let shared_ws = dir.path().join("job-2");
    std::fs::create_dir_all(&shared_ws).unwrap();
    prepare_job_docker_config_dir(shared_ws.to_str().unwrap(), DockerConfigIsolation::Shared)
        .unwrap();
    assert!(!shared_ws.join(".docker").exists());
}

#[test]
fn clean_job_docker_config_removes_dir_and_tolerates_missing() {
    let dir = tempfile::tempdir().unwrap();
    let job_ws = dir.path().join("job-1");
    let docker_dir = job_ws.join(".docker");
    std::fs::create_dir_all(&docker_dir).unwrap();
    std::fs::write(docker_dir.join("config.json"), "{}").unwrap();

    clean_job_docker_config(job_ws.to_str().unwrap());
    assert!(!docker_dir.exists(), "config dir must be removed");

    // Missing dir (e.g. workspace already cleaned) is not an error.
    clean_job_docker_config(job_ws.to_str().unwrap());
    // The job workspace itself survives — only .docker is removed.
    assert!(job_ws.exists());
}

#[test]
fn ssh_env_command_creates_docker_config_dir_before_exports() {
    let mut env = HashMap::new();
    env.insert(
        "DOCKER_CONFIG".to_string(),
        "/tmp/buz-docker-job-1".to_string(),
    );
    let cmd = build_ssh_env_command("docker pull reg.example/app:1\n", &env);
    assert!(cmd.starts_with("set -e; "), "{cmd}");
    let mkdir_pos = cmd
        .find("mkdir -p '/tmp/buz-docker-job-1'")
        .expect("mkdir prefix");
    let export_pos = cmd
        .find("export DOCKER_CONFIG=")
        .expect("docker config export");
    assert!(mkdir_pos < export_pos, "mkdir must precede exports: {cmd}");
}

// ── Cancel delivery over the claim channel ──

#[test]
fn parse_poll_data_splits_job_and_cancel_notices() {
    // Cancel-only payload (no-job paths: idle / frozen / capacity gate).
    let out = super::parse_poll_data(serde_json::json!({
        "cancelled_jobs": ["j1", "j2"],
    }));
    assert!(out.task.is_none());
    assert_eq!(out.cancelled_jobs, vec!["j1".to_string(), "j2".to_string()]);

    // Claim payload carries notices alongside the job.
    let out = super::parse_poll_data(serde_json::json!({
        "job_id": "j3",
        "cancelled_jobs": ["j1"],
    }));
    assert_eq!(out.task.unwrap()["job_id"], "j3");
    assert_eq!(out.cancelled_jobs, vec!["j1".to_string()]);

    // Defensive: null data and missing fields parse to nothing.
    let out = super::parse_poll_data(serde_json::Value::Null);
    assert!(out.task.is_none() && out.cancelled_jobs.is_empty());
    let out = super::parse_poll_data(serde_json::json!({}));
    assert!(out.task.is_none() && out.cancelled_jobs.is_empty());
}

#[tokio::test]
async fn registry_signals_live_jobs_and_ignores_unknown_ids() {
    let registry: super::CancelRegistry = Default::default();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    registry.lock().unwrap().insert("job-1".to_string(), tx);

    // Unknown ids (already finished locally) are dropped silently.
    super::signal_cancel(&registry, "gone-job");

    super::signal_cancel(&registry, "job-1");
    assert!(super::cancel_triggered(&mut rx).await, "live job signalled");

    // The registry consumed the entry — a repeat notice finds nothing.
    super::signal_cancel(&registry, "job-1");
}

#[tokio::test]
async fn cancel_triggered_parks_when_sender_drops_without_signal() {
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    drop(tx);

    // Sender dropped, no cancellation latched: the arm must stay silent so
    // the step future's own completion ends any select race.
    let mut fired = false;
    tokio::select! {
        _ = super::cancel_triggered(&mut rx) => fired = true,
        _ = tokio::time::sleep(Duration::from_millis(50)) => {}
    }
    assert!(!fired, "dropped sender without signal must never fire");
}

use std::time::Duration;
