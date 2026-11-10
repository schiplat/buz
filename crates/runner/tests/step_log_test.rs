//! Tests for the step-log formatting layer (`super`): chunk layout
//! arithmetic, banner builders, and content redaction pins.

use runner::client::poll::step_log::*;
use runner::masker::SecretCatalog;
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
    let cfg = build_verbose_step_config(&step, "ssh");
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
        "BUZ_DYNAMIC_VAR".into(),
        "http://internal.example/signed?sig=abc".into(),
    );
    env.insert("acr_personal".into(), "secret-json".into());
    let mut catalog = SecretCatalog::new();
    catalog.add_injected_key("BUZ_DYNAMIC_VAR");
    catalog.add_key("acr_personal");

    let echo = build_step_echo("shell", "echo hi\n", &env, &catalog, None);
    assert!(echo.contains("BUZ_DYNAMIC_VAR=<injected>"), "{echo}");
    assert!(echo.contains("acr_personal=***"), "{echo}");
    assert!(!echo.contains("http://internal.example"), "{echo}");
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
fn stream_window_stops_before_sentinels() {
    // The stream must never hand out an index the sentinels use: job_logs is
    // UNIQUE(job_id, chunk_index) + INSERT OR REPLACE, so a collision silently
    // overwrites the truncation notice, the ERROR chunk, or the summary.
    let base = step_log_base(1);
    let mut budget = StreamBudget::new(base);
    let mut handed_out = 0;
    while let Some(index) = budget.take() {
        assert!(
            index <= base + LOG_STREAM_LAST,
            "budget handed out sentinel-reserved index {index}"
        );
        assert!(
            index < base + LOG_STEP_TRUNCATED,
            "stream index {index} collides with the truncation marker"
        );
        handed_out += 1;
    }
    // Window is exactly LOG_STREAM_START..=LOG_STREAM_LAST, one index per chunk.
    assert_eq!(
        handed_out,
        LOG_STREAM_LAST - LOG_STREAM_START + 1,
        "window width"
    );
    assert_eq!(budget.take(), None, "no further index past the window");
    assert!(
        !budget.truncated(),
        "state belongs to omit(): dropping a chunk is what flips it"
    );
    budget.omit("dropped\n");
    assert!(budget.truncated());
}

#[test]
fn stream_window_never_crosses_into_next_step() {
    // Every step owns 10_000 indexes; a step's stream reaching LOG_STREAM_LAST
    // must still sort strictly before the next step's first chunk.
    let step0_last = step_log_base(0) + LOG_STREAM_LAST;
    let step1_first = step_log_base(1) + LOG_STEP_VERBOSE_PREPARE;
    assert!(
        step0_last < step1_first,
        "stream must stay inside its own step"
    );
    assert!(step_log_base(0) + LOG_STEP_SUMMARY < step1_first);
}

#[test]
fn truncation_marker_is_warn_and_reports_omitted_count() {
    assert_eq!(
        level_for_offset(LOG_STEP_TRUNCATED),
        shared::protocol::LogLevel::Warn
    );
    let mut budget = StreamBudget::new(step_log_base(0));
    // Fill the window exactly — nothing is omitted yet.
    let width = LOG_STREAM_LAST - LOG_STREAM_START + 1;
    for _ in 0..width {
        assert!(budget.take().is_some());
    }
    assert!(!budget.truncated(), "full window is not yet truncated");
    // Three chunks past the window are dropped, counted, and kept in the tail.
    for i in 0..3 {
        assert_eq!(budget.take(), None);
        budget.omit(&format!("overflow line {i}\n"));
    }
    assert!(budget.truncated());
    let marker = budget.marker();
    assert!(marker.contains("OUTPUT TRUNCATED"), "{marker}");
    assert!(
        marker.contains("3 chunk"),
        "omitted count in marker: {marker}"
    );
    assert!(
        marker.contains("kept     first 9985 chunk(s)"),
        "kept head stated: {marker}"
    );
    assert!(
        marker.contains("last 3 line(s)"),
        "kept tail stated: {marker}"
    );
    assert!(marker.contains("overflow line 2"), "tail present: {marker}");
}

#[test]
fn truncation_is_announced_once_then_kept_for_the_final_marker() {
    let mut budget = StreamBudget::new(step_log_base(0));
    assert!(
        budget.unnotified_truncation().is_none(),
        "no notice while inside the window"
    );
    // Fill the window without triggering the overflow path.
    let width = LOG_STREAM_LAST - LOG_STREAM_START + 1;
    for _ in 0..width {
        assert!(budget.take().is_some());
    }
    assert!(
        budget.unnotified_truncation().is_none(),
        "no notice for a stream that exactly fills the window"
    );
    // First overflow: counted and announced once.
    assert_eq!(budget.take(), None);
    budget.omit("first\n");
    assert!(
        budget.unnotified_truncation().is_some(),
        "first overflow surfaces a live notice"
    );
    for _ in 0..5 {
        assert_eq!(budget.take(), None);
        budget.omit("more\n");
    }
    assert!(
        budget.unnotified_truncation().is_none(),
        "live notice is emitted exactly once"
    );
    assert!(
        budget.marker().contains("6 chunk"),
        "final marker carries the total omitted count: {}",
        budget.marker()
    );
}

#[test]
fn retry_marker_names_attempt_and_detail() {
    let marker = build_retry_marker("build", 1, 3, "exit 1");
    assert!(marker.contains("RETRY 1/2"), "{marker}");
    assert!(marker.contains("step  build"), "{marker}");
    assert!(marker.contains("detail  exit 1"), "{marker}");
    assert_eq!(
        level_for_offset(LOG_STEP_RETRY),
        shared::protocol::LogLevel::Warn
    );
}

#[test]
fn stream_budget_resumes_after_a_previous_attempt() {
    let base = step_log_base(0);
    let mut budget = StreamBudget::new_at(base, base + 7);
    assert_eq!(budget.take(), Some(base + 7));
    assert_eq!(budget.take(), Some(base + 8));
    assert_eq!(budget.next_index(), base + 9);
}

#[test]
fn truncation_marker_does_not_collide_with_verbose_or_error_chunks() {
    let mut set = std::collections::HashSet::new();
    for offset in [
        LOG_STREAM_LAST,
        LOG_STEP_TRUNCATED,
        LOG_STEP_RETRY,
        LOG_STEP_VERBOSE_RESULT,
        LOG_STEP_ERROR,
        LOG_STEP_CANCELLED,
        LOG_STEP_SUMMARY,
    ] {
        assert!(
            set.insert(offset),
            "offset {offset} collides with the truncation window"
        );
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
        "config_dest": "/opt/manager",
        "config_render": [{ "template": "env.example", "output": ".env" }],
        "hosts": ["h1", "h2"],
        "image": "",
    });
    let cfg = build_verbose_step_config(&step, "shell");
    assert!(cfg.starts_with("==> STEP CONFIG\n"), "{cfg}");
    assert!(cfg.contains("  exec_type        shell"), "{cfg}");
    assert!(cfg.contains("  timeout          3600s"), "{cfg}");
    assert!(cfg.contains("  fail_strategy    stop"), "{cfg}");
    assert!(cfg.contains("  work_dir         /tmp/ws"), "{cfg}");
    assert!(cfg.contains("  config_dest      /opt/manager"), "{cfg}");
    assert!(
        cfg.contains("  config_render    env.example → .env"),
        "{cfg}"
    );
    assert!(cfg.contains("  hosts            h1, h2"), "{cfg}");
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
fn chunk_levels_follow_layout_offsets() {
    use shared::protocol::LogLevel as L;
    assert_eq!(level_for_offset(LOG_STEP_ERROR), L::Error);
    assert_eq!(level_for_offset(LOG_STEP_CANCELLED), L::Warn);
    for offset in [
        LOG_STEP_VERBOSE_PREPARE,
        LOG_STEP_VERBOSE_CONFIG,
        LOG_STEP_VERBOSE_CONFIG_PREP,
        LOG_STEP_VERBOSE_EXEC,
        LOG_STEP_VERBOSE_RESULT,
    ] {
        assert_eq!(level_for_offset(offset), L::Debug, "offset {offset}");
    }
    for offset in [LOG_LOGIN, LOG_ECHO, LOG_STREAM_START, LOG_STEP_SUMMARY] {
        assert_eq!(level_for_offset(offset), L::Info, "offset {offset}");
    }
}

#[test]
fn truncation_marker_carries_a_bounded_masked_tail() {
    let mut budget = StreamBudget::new(step_log_base(0));
    let width = LOG_STREAM_LAST - LOG_STREAM_START + 1;
    for _ in 0..width {
        budget.take();
    }
    // Overflow: every drained chunk goes through take() (counting the
    // omission) and then into the tail — mirroring stream_chunks.
    for i in 0..250u32 {
        assert_eq!(budget.take(), None);
        budget.omit(&format!("overflow line {i}\n"));
    }
    let marker = budget.marker();
    assert!(marker.contains("omitted  250 chunk(s)"), "{marker}");
    assert!(marker.contains("last 200 line(s)"), "{marker}");
    assert!(
        marker.contains("overflow line 249"),
        "keeps the end: {marker}"
    );
    assert!(
        !marker.contains("overflow line 49\n"),
        "drops the head past the window: {marker}"
    );
    // Long lines are clipped.
    let mut budget = StreamBudget::new(step_log_base(0));
    while budget.take().is_some() {}
    let long = "x".repeat(5000);
    let _ = budget.take();
    budget.omit(&format!("{long}\n"));
    let marker = budget.marker();
    assert!(marker.len() < long.len(), "tail line clipped: {marker}");
    assert!(marker.contains('…'), "clip marker present");
}

#[test]
fn stream_outcome_carries_the_omitted_count() {
    let base = step_log_base(0);
    let mut budget = StreamBudget::new(base);
    let width = LOG_STREAM_LAST - LOG_STREAM_START + 1;
    for _ in 0..width {
        assert!(budget.take().is_some());
    }
    assert_eq!(budget.omitted(), None, "full window is not truncated");
    budget.take();
    assert_eq!(budget.omitted(), None, "take() alone does not count");
    budget.omit("chunk one\n");
    assert_eq!(budget.omitted(), Some(1));
    budget.take();
    budget.omit("chunk two\n");
    assert_eq!(budget.omitted(), Some(2));
}

#[test]
fn truncation_note_states_the_incomplete_output() {
    let note = runner::client::poll::step_log::build_truncation_note(1234);
    assert!(note.contains("OUTPUT TRUNCATED"), "{note}");
    assert!(note.contains("1234"), "{note}");
    assert!(note.contains("incomplete"), "{note}");
}

#[test]
fn marker_states_kept_head_kept_tail_and_volume() {
    let mut budget = StreamBudget::new(step_log_base(0));
    let width = LOG_STREAM_LAST - LOG_STREAM_START + 1;
    for _ in 0..width {
        budget.take();
    }
    // 1 KiB of dropped output in two chunks, plus a 3 KiB line that gets clipped.
    budget.omit(&format!("{}\n", "a".repeat(600)));
    budget.omit(&format!("{}\n", "b".repeat(600)));
    let marker = budget.marker();
    assert!(marker.contains("kept     first 9985 chunk(s)"), "{marker}");
    assert!(marker.contains("kept     last 2 line(s)"), "{marker}");
    assert!(
        marker.contains("omitted  2 chunk(s) in between"),
        "{marker}"
    );
    assert!(marker.contains("≈ 1.2 KiB"), "volume reported: {marker}");

    // A clipped line says so, instead of masquerading  as  a genuinely long one.
    let mut budget = StreamBudget::new_at(step_log_base(0), step_log_base(0) + LOG_STREAM_LAST);
    budget.take();
    budget.omit(&format!("{}\n", "x".repeat(5000)));
    let marker = budget.marker();
    assert!(marker.contains("(line truncated)"), "{marker}");
    assert!(
        marker.len() < 5000,
        "tail line clipped: {} bytes",
        marker.len()
    );
}
