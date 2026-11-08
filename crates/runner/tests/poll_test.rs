use runner::client::poll::{
    build_ssh_env_command, buz_metadata_env, clean_job_docker_config, job_docker_config_dir,
    merge_ssh_host_outputs, parse_buz_env_file, prepare_job_docker_config_dir,
};
use std::collections::HashMap;

#[test]
fn work_dir_rejects_absolute_parent_and_prefix_escape() {
    assert!(!runner::client::poll::is_safe_workspace_subdir("/tmp"));
    assert!(!runner::client::poll::is_safe_workspace_subdir(
        "../outside"
    ));
    assert!(!runner::client::poll::is_safe_workspace_subdir(
        "src/../../outside"
    ));
    assert!(!runner::client::poll::is_safe_workspace_subdir(""));
    assert!(runner::client::poll::is_safe_workspace_subdir(
        "frontend/src"
    ));
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
    let mut runner = runner::config::RunnerSection::default();
    runner.docker_mount_policy = runner::config::DockerMountPolicy::Strict;
}

#[test]
fn merge_ssh_host_outputs_any_failure_fails_step() {
    let out = merge_ssh_host_outputs(
        "Deploy",
        true,
        vec![
            (
                "host-b".into(),
                Ok(runner::engine::ssh::SshOutput {
                    stdout: "ok-b\n".into(),
                    stderr: String::new(),
                    exit_code: 0,
                }),
            ),
            (
                "host-a".into(),
                Ok(runner::engine::ssh::SshOutput {
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

// ── Execution-path output equivalence ──────────────────────────────────────
//
// These pins guard the step-log contract shared by all three execution paths
// (local shell / docker streaming, sandbox streaming, SSH summary):
// chunk layout from `LOG_STREAM_START`, secret masking before any upload, and
// the ERROR/CANCELLED banner shapes. They must keep passing while the upload
// boilerplate is refactored behind StepLogger.

/// Read one full HTTP request (headers + content-length body) from the stream.
async fn read_http_request(stream: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt;
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

#[tokio::test]
async fn local_shell_step_streams_masked_chunks_and_reports_success() {
    use std::sync::Mutex as StdMutex;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let bodies: std::sync::Arc<StdMutex<Vec<String>>> =
        std::sync::Arc::new(StdMutex::new(Vec::new()));
    let server_bodies = bodies.clone();
    let server = tokio::spawn(async move {
        for _ in 0..32 {
            // Idle timeout: once the runner stops connecting, end the loop so
            // the test's `server.await` cannot park forever.
            let accepted =
                tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept()).await;
            let (mut stream, _) = match accepted {
                Ok(Ok(inner)) => inner,
                _ => break,
            };
            let request = read_http_request(&mut stream).await;
            server_bodies.lock().unwrap().push(request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        }
    });

    let client = reqwest::Client::new();
    let config = runner::config::Config {
        runner: runner::config::RunnerSection {
            manager_url: format!("http://{addr}"),
            ..Default::default()
        },
        ..Default::default()
    };
    let task = serde_json::json!({
        "job_id": "job-stream-equivalence",
        "secrets": { "TOKEN": "buz-super-secret-value" },
        "env": { "TOKEN": "buz-super-secret-value" },
        "tasks": [{
            "name": "echo",
            "command_template": "printf '%s\\n' \"$TOKEN\"",
            "exec_type": "shell"
        }]
    });
    let (_cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    let result =
        runner::client::poll::execute_job(&client, &config, "runner-token", &task, &mut cancel_rx)
            .await;
    assert!(result.is_ok(), "job must succeed: {result:?}");

    let requests = bodies.lock().unwrap().clone();
    // banner → running → streamed chunk(s) → success
    assert!(requests[0].starts_with("POST /api/v1/jobs/job-stream-equivalence/logs "));
    assert!(
        requests[0].contains("\"chunk_index\":-2"),
        "{}",
        requests[0]
    );
    assert!(
        requests[1].contains("\"status\":\"running\""),
        "{}",
        requests[1]
    );
    let stream_chunks: Vec<&String> = requests
        .iter()
        .filter(|r| {
            r.contains("POST /api/v1/jobs/job-stream-equivalence/logs ")
                && !r.contains("\"chunk_index\":-2")
        })
        .collect();
    assert!(
        !stream_chunks.is_empty(),
        "streamed output must be uploaded: {requests:?}"
    );
    assert!(
        stream_chunks
            .iter()
            .all(|r| !r.contains("buz-super-secret-value")),
        "stream chunks must be masked: {stream_chunks:?}"
    );
    assert!(
        requests
            .iter()
            .any(|r| r.contains("chunk_index\":6") && r.contains("***")),
        "first stream chunk starts at LOG_STREAM_START and is masked: {requests:?}"
    );
    assert!(
        !requests
            .iter()
            .any(|r| r.contains("buz-super-secret-value")),
        "the raw secret must never reach the wire: {requests:?}"
    );
    let last = requests.last().unwrap();
    assert!(
        last.contains("PATCH") && last.contains("\"status\":\"success\""),
        "{last}"
    );
    server.await.unwrap();
}

#[test]
fn sandbox_masker_split_feeding_equals_one_shot_masking() {
    // Sandbox streaming property: feeding the same buffer in arbitrary frame
    // splits through SandboxOutputMasker yields exactly the masked output of
    // masking the whole buffer once — for every split point, including inside
    // secrets (hold-back must prevent boundary leaks).
    let secret = "s3cret-value";
    let forced = "forcedtok";
    let body = format!("a {secret} b {forced} c {secret} tail");
    let expected = runner::masker::mask_secrets_forced(
        &runner::masker::mask_secrets(&body, &[secret.to_string()]),
        &[forced.to_string()],
    );
    assert_ne!(expected, body, "sanity: masking must change the body");
    for split in 1..body.len() {
        let mut masker = runner::client::poll::SandboxOutputMasker::new(
            &[secret.to_string()],
            &[forced.to_string()],
        );
        let mut got = masker.push(&body[..split], false);
        got.push_str(&masker.push(&body[split..], true));
        assert_eq!(got, expected, "mask equivalence broken at split {split}");
    }
}

#[tokio::test]
async fn marketplace_input_validation_error_logs_and_fails_job_before_execution() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

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
    let config = runner::config::Config {
        runner: runner::config::RunnerSection {
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
    let error =
        runner::client::poll::execute_job(&client, &config, "runner-token", &task, &mut cancel_rx)
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
    use runner::config::DockerConfigIsolation;

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
    let out = runner::client::poll::parse_poll_data(serde_json::json!({
        "cancelled_jobs": ["j1", "j2"],
    }));
    assert!(out.task.is_none());
    assert_eq!(out.cancelled_jobs, vec!["j1".to_string(), "j2".to_string()]);

    // Claim payload carries notices alongside the job.
    let out = runner::client::poll::parse_poll_data(serde_json::json!({
        "job_id": "j3",
        "cancelled_jobs": ["j1"],
    }));
    assert_eq!(out.task.unwrap()["job_id"], "j3");
    assert_eq!(out.cancelled_jobs, vec!["j1".to_string()]);

    // Defensive: null data and missing fields parse to nothing.
    let out = runner::client::poll::parse_poll_data(serde_json::Value::Null);
    assert!(out.task.is_none() && out.cancelled_jobs.is_empty());
    let out = runner::client::poll::parse_poll_data(serde_json::json!({}));
    assert!(out.task.is_none() && out.cancelled_jobs.is_empty());
}

#[tokio::test]
async fn registry_signals_live_jobs_and_ignores_unknown_ids() {
    let registry: runner::client::poll::CancelRegistry = Default::default();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    registry.lock().unwrap().insert("job-1".to_string(), tx);

    // Unknown ids (already finished locally) are dropped silently.
    runner::client::poll::signal_cancel(&registry, "gone-job");

    runner::client::poll::signal_cancel(&registry, "job-1");
    assert!(
        runner::client::poll::cancel_triggered(&mut rx).await,
        "live job signalled"
    );

    // The registry consumed the entry — a repeat notice finds nothing.
    runner::client::poll::signal_cancel(&registry, "job-1");
}

#[tokio::test]
async fn cancel_triggered_parks_when_sender_drops_without_signal() {
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    drop(tx);

    // Sender dropped, no cancellation latched: the arm must stay silent so
    // the step future's own completion ends any select race.
    let mut fired = false;
    tokio::select! {
        _ = runner::client::poll::cancel_triggered(&mut rx) => fired = true,
        _ = tokio::time::sleep(Duration::from_millis(50)) => {}
    }
    assert!(!fired, "dropped sender without signal must never fire");
}

use std::time::Duration;

#[tokio::test]
async fn sandbox_modes_reject_ssh_before_contacting_supervisor() {
    let config = runner::config::Config {
        execution_mode: shared::protocol::RunnerExecutionMode::Container,
        ..Default::default()
    };
    let task = serde_json::json!({
        "job_id": "unsupported-ssh-job",
        "tasks": [{ "name": "remote", "exec_type": "ssh", "command_template": "echo hi" }]
    });
    let (_cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    let error = runner::client::poll::execute_job(
        &reqwest::Client::new(),
        &config,
        "token",
        &task,
        &mut cancel_rx,
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("does not support SSH"),
        "{error}"
    );
}

#[tokio::test]
async fn sandbox_mode_fails_closed_when_supervisor_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("job-shell");
    std::fs::create_dir(&workspace).unwrap();
    let mut config = runner::config::Config {
        execution_mode: shared::protocol::RunnerExecutionMode::Container,
        ..Default::default()
    };
    config.runner.workspace_dir = workspace.to_string_lossy().into_owned();
    config.sandbox.workspace_root = dir.path().to_string_lossy().into_owned();
    config.sandbox.supervisor_socket = dir
        .path()
        .join("missing.sock")
        .to_string_lossy()
        .into_owned();
    let task = serde_json::json!({
        "job_id": "supervisor-down-job",
        "tasks": [{ "name": "local", "exec_type": "shell", "command_template": "echo must-not-run" }]
    });
    let (_cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    let error = runner::client::poll::execute_job(
        &reqwest::Client::new(),
        &config,
        "token",
        &task,
        &mut cancel_rx,
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("No such file")
            || error.to_string().contains("sandbox supervisor"),
        "{error}"
    );
}

#[test]
fn active_runner_uses_short_poll_to_refresh_capacity_snapshot() {
    assert_eq!(runner::client::poll::poll_wait_seconds(0), 45);
    assert_eq!(runner::client::poll::poll_wait_seconds(1), 2);
    assert_eq!(runner::client::poll::poll_wait_seconds(8), 2);
}
