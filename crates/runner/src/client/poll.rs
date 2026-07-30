use reqwest::Client;
use shared::protocol::{LogChunk, UpdateJobStatusRequest};
use std::path::Path;
use std::time::Duration;

use crate::config::Config;
use crate::engine;
use std::collections::{HashMap, HashSet};

/// Build a `bash -x` style echo line showing the command with its environment.
/// Secret values (from pipeline/key-level secrets) are masked as `***`.
fn build_step_echo(exec_type: &str, command: &str, env: &HashMap<String, String>, secret_keys: &HashSet<String>, work_dir: Option<&str>) -> String {
    let mut parts = vec![format!("==> RUN [{}]", exec_type)];
    parts.push("note: GIT_TERMINAL_PROMPT=0 (noninteractive) — interactive prompts will cause failure".to_string());
    if let Some(wd) = work_dir {
        parts.push(format!("cwd: {wd}"));
    }
    if !env.is_empty() {
        let mut items: Vec<String> = Vec::new();
        for (k, v) in env {
            if secret_keys.contains(k) {
                items.push(format!("{k}=***"));
            } else {
                items.push(format!("{k}={v}"));
            }
        }
        parts.push(format!("env: [{}]", items.join(", ")));
    }
    parts.push(format!("cmd: {command}"));
    parts.join("\n") + "\n"
}

/// Poll the manager to check if this job has been cancelled.
/// Returns `true` if the job is cancelled and execution should stop.
pub async fn check_job_cancelled(
    client: &Client,
    config: &Config,
    token: &str,
    job_id: &str,
) -> anyhow::Result<bool> {
    let resp = client
        .get(&format!(
            "{}/api/v1/jobs/{job_id}/status-check",
            config.manager.url
        ))
        .header("Authorization", format!("Bearer {token}"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| {
            super::friendly_reqwest_error(e, &config.manager.url, "Status check failed")
        })?;

    if !resp.status().is_success() {
        return Ok(false);
    }

    let body = resp.json::<serde_json::Value>().await?;
    let status = body["data"]["status"].as_str().unwrap_or("");

    Ok(status == "cancelled")
}

/// Poll loop with exponential backoff.
/// On each successful poll response (task or no task), the backoff resets.
/// On failure the backoff doubles (capped at 60s), so the runner degrades
/// gracefully instead of hammering the Center.
pub async fn run_poll_loop(client: &Client, config: &Config, token: &str) -> anyhow::Result<()> {
    let mut consecutive_failures: u32 = 0;

    loop {
        match poll_for_task(client, config, token).await {
            Ok(Some(task)) => {
                // Success — reset backoff and execute
                consecutive_failures = 0;

                let job_id = task["job_id"].as_str().unwrap_or("unknown");
                tracing::info!("Executing job {job_id}");

                if let Err(e) = execute_job(client, config, token, &task).await {
                    let msg = e.to_string();
                    if msg.contains("job cancelled by user") {
                        tracing::warn!("Job {job_id} cancelled by user — skipping error log");
                        if let Err(se) =
                            update_job_status(client, config, token, job_id, "cancelled", None)
                                .await
                        {
                            tracing::error!("CRITICAL: Failed to report cancelled status for job {job_id}: {se}");
                        }
                    } else {
                        tracing::error!("Job {job_id} failed: {e}");
                        // Upload the error message so it appears in the live terminal
                        if let Err(le) = super::logs::upload_chunk(
                            client,
                            config,
                            token,
                            job_id,
                            LogChunk {
                                chunk_index: -1,
                                step_order: -1,
                                content: format!("==> ERROR <==\n{e}\n"),
                            },
                        )
                        .await
                        {
                            tracing::warn!("Job {job_id}: failed to upload error log chunk: {le}");
                        }
                        if let Err(se) =
                            update_job_status(client, config, token, job_id, "failed", None).await
                        {
                            tracing::error!(
                                "CRITICAL: Failed to report failed status for job {job_id}: {se}"
                            );
                        }
                    }
                }

                // ── Ephemeral / workspace cleanup ─────────────────────────────
                if config.runner.clean_workspace_after_job || config.runner.ephemeral {
                    tracing::info!("Cleaning workspace: {}", config.runner.workspace_dir);
                    clean_workspace(&config.runner.workspace_dir);
                }

                if config.runner.ephemeral {
                    tracing::info!(
                        "Ephemeral runner exiting after job {job_id} — container/VM will be destroyed"
                    );
                    return Ok(());
                }
                // ───────────────────────────────────────────────────────────────
            }
            Ok(None) => {
                // No task — this is a normal poll, reset backoff
                if consecutive_failures > 0 {
                    tracing::info!("Poll recovered after {consecutive_failures} failure(s)");
                }
                consecutive_failures = 0;
            }
            Err(e) => {
                consecutive_failures += 1;

                let delay_secs = match consecutive_failures {
                    0 => 0,                         // unreachable in Err branch
                    n if n <= 5 => 2u64 << (n - 1), // 2, 4, 8, 16, 32
                    _ => 60,                        // cap
                };
                let delay = Duration::from_secs(delay_secs);

                tracing::warn!(
                    "Poll failed (attempt #{consecutive_failures}): {e}.  Retrying in {delay:?}..."
                );

                tokio::time::sleep(delay).await;
            }
        }
    }
}

async fn poll_for_task(
    client: &Client,
    config: &Config,
    token: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    let resp = client
        .get(&format!("{}/api/v1/runners/poll", config.manager.url))
        .header("Authorization", format!("Bearer {token}"))
        .timeout(Duration::from_secs(50))
        .send()
        .await
        .map_err(|e| super::friendly_reqwest_error(e, &config.manager.url, "Poll failed"))?;

    if resp.status() == 204 {
        return Ok(None);
    }

    if !resp.status().is_success() {
        // Distinguish auth failures from transient errors
        let status = resp.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            anyhow::bail!("auth error ({status}) — token may have expired, re‑register needed");
        }
        anyhow::bail!("poll returned {status}");
    }

    let body = resp.json::<serde_json::Value>().await?;

    let data = body.get("data").cloned().unwrap_or_default();
    if data.is_null() {
        return Ok(None);
    }

    Ok(Some(data))
}

async fn execute_job(
    client: &Client,
    config: &Config,
    token: &str,
    task: &serde_json::Value,
) -> anyhow::Result<()> {
    let job_id = task["job_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing job_id"))?;

    let tasks = task["tasks"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing tasks array"))?;

    // Build base env from trigger inputs + pipeline-level secrets.
    // Step-level secrets are merged per-step below.
    let mut base_env: std::collections::HashMap<String, String> = task
        .get("inputs")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    // Merge pipeline-level resolved secrets from manager (e.g. env vars, platform vars, pipeline secrets)
    let mut pipeline_secret_keys: HashSet<String> = HashSet::new();
    if let Some(secrets_obj) = task.get("secrets").and_then(|v| v.as_object()) {
        for (k, v) in secrets_obj {
            if let Some(s) = v.as_str() {
                base_env.insert(k.clone(), s.to_string());
                pipeline_secret_keys.insert(k.clone());
            }
        }
    }

    // Force non-interactive mode for common tools. When a step tries to read
    // from stdin (e.g. `git clone` prompting for credentials), these env vars
    // make the tool fail immediately with a clear error instead of hanging.
    base_env.entry("GIT_TERMINAL_PROMPT".to_string()).or_insert_with(|| "0".to_string());
    base_env.entry("DEBIAN_FRONTEND".to_string()).or_insert_with(|| "noninteractive".to_string());

    // ---- Cancel watcher helper -------------------------------------------------
    /// Returns `true` when the job has been cancelled.
    /// Polls the Manager every 2 s.
    async fn cancel_watcher(client: &Client, config: &Config, token: &str, job_id: &str) -> bool {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if check_job_cancelled(client, config, token, job_id)
                .await
                .unwrap_or(false)
            {
                return true;
            }
        }
    }
    // ---------------------------------------------------------------------------

    for (idx, step) in tasks.iter().enumerate() {
        let step_name = step["name"].as_str().unwrap_or("unnamed");
        let command = step["command_template"].as_str().unwrap_or("");
        let work_dir = step["work_dir"].as_str().filter(|s| !s.is_empty());
        let exec_type = step["exec_type"].as_str().unwrap_or("shell");
        let fail_strategy = step["fail_strategy"].as_str().unwrap_or("stop");
        let step_timeout_secs = step["timeout_seconds"].as_u64().unwrap_or(0);
        let step_timeout = if step_timeout_secs > 0 {
            Some(Duration::from_secs(step_timeout_secs))
        } else {
            None
        };
        let has_timeout = step_timeout.is_some();

        // ── Build step-specific env: base (inputs+pipeline-secrets) + step-level secrets ──
        let mut step_env = base_env.clone();
        let mut step_secret_keys = pipeline_secret_keys.clone();
        if let Some(ss_obj) = step.get("step_secrets").and_then(|v| v.as_object()) {
            for (k, v) in ss_obj {
                if let Some(s) = v.as_str() {
                    step_env.insert(k.clone(), s.to_string());
                    step_secret_keys.insert(k.clone());
                }
            }
        }

        // Collect all secret values for log masking (GitHub Actions-style redaction)
        let secret_values = crate::masker::collect_secret_values(&step_env);

        if let Err(e) = config.engine.ensure_enabled(exec_type) {
            let msg = e.to_string();
            let _ = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: (idx * 10000 + 9997) as i32,
                    step_order: idx as i32,
                    content: format!("==> ERROR <==\n{msg}\n"),
                },
            )
            .await;
            update_job_status(client, config, token, job_id, "failed", Some(idx as i32)).await?;
            anyhow::bail!(msg);
        }

        let env_count = step_env.len();
        tracing::info!(
            "Step {idx}: {step_name} ({exec_type}){}",
            if env_count > 0 {
                format!(" [with {env_count} env vars]")
            } else {
                String::new()
            }
        );

        update_job_status(client, config, token, job_id, "running", Some(idx as i32)).await?;

        // ── Race step execution against cancel polling ────────────────────────
        let (output, was_cancelled): (engine::shell::ShellOutput, bool) = match exec_type {
            "local" | "shell" => {
                let cmd = command.to_string();
                let wd = work_dir.map(|s| s.to_string());
                let env = step_env.clone();

                // Debug echo — show the command before execution (like bash -x)
                {
                    let echo_line = build_step_echo(exec_type, &cmd, &env, &step_secret_keys, work_dir);
                    let _ = super::logs::upload_chunk(
                        client, config, token, job_id,
                        LogChunk { chunk_index: 0, step_order: idx as i32, content: echo_line },
                    ).await;
                }

                let c = client.clone();
                let cfg = config.clone();
                let t = token.to_string();
                let jid = job_id.to_string();
                let sname = step_name.to_string();

                let (mut child, mut output_rx, read_guard) =
                    engine::shell::execute_streaming(&cmd, wd.as_deref(), Some(&env)).await?;

                // Clone secret values for use in the async upload task
                let sv_for_upload = secret_values.clone();

                // Background task: stream output chunks to the manager in real time
                let c_log = client.clone();
                let cfg_log = config.clone();
                let t_log = token.to_string();
                let jid_log = job_id.to_string();
                let idx_i32 = idx as i32;
                let upload_task = tokio::spawn(async move {
                    let mut chunk_num = 1i32;
                    while let Some(chunk) = output_rx.recv().await {
                        let masked_chunk = crate::masker::mask_secrets(&chunk, &sv_for_upload);
                        match super::logs::upload_chunk(
                            &c_log,
                            &cfg_log,
                            &t_log,
                            &jid_log,
                            LogChunk {
                                chunk_index: chunk_num,
                                step_order: idx_i32,
                                content: masked_chunk,
                            },
                        )
                        .await
                        {
                            Ok(_) => {}
                            Err(e) => tracing::warn!("Log upload chunk {chunk_num} failed: {e}"),
                        }
                        chunk_num += 1;
                    }
                });

                // Race: child completion vs cancellation vs timeout
                let (exit_code, was_cancelled_local, was_timeout) = tokio::select! {
                    status = child.wait() => {
                        let code = status?.code().unwrap_or(-1);
                        // Ensure all pipe data is read before proceeding
                        let _ = read_guard.join().await;
                        let _ = upload_task.await;
                        (code, false, false)
                    }
                    _ = cancel_watcher(&c, &cfg, &t, &jid) => {
                        tracing::warn!("Job {jid} cancelled during step '{sname}' — killing");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        upload_task.abort();
                        (-1, true, false)
                    }
                    _ = tokio::time::sleep(step_timeout.unwrap_or(Duration::from_secs(3600))), if has_timeout => {
                        tracing::warn!("Step '{sname}' timed out after {step_timeout_secs}s — killing process");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        let _ = read_guard.join().await;
                        upload_task.abort();
                        (-1, false, true)
                    }
                };

                (
                    engine::shell::ShellOutput {
                        stdout: String::new(),
                        stderr: if was_cancelled_local {
                            "==> CANCELLED <==\n".into()
                        } else if was_timeout {
                            format!("==> TIMEOUT after {step_timeout_secs}s <==\n")
                        } else {
                            String::new()
                        },
                        exit_code: if was_cancelled_local || was_timeout { -1 } else { exit_code },
                    },
                    was_cancelled_local,
                )
            }
            "ssh" => {
                let host_values = step["host"]
                    .as_array()
                    .map(|a| a.clone())
                    .or_else(|| {
                        step["host"]
                            .as_object()
                            .map(|o| vec![serde_json::Value::Object(o.clone())])
                    })
                    .unwrap_or_else(|| {
                        let _msg =
                            format!("SSH step '{step_name}' missing host connection details");
                        vec![]
                    });

                if host_values.is_empty() {
                    let msg = if step["host"].is_null() {
                        format!("SSH step '{step_name}' missing host connection details")
                    } else {
                        format!("SSH step '{step_name}': no hosts resolved from the database. Register your hosts in the Manager UI first.")
                    };
                    // Upload the error as a log chunk so it appears in the step's terminal
                    let _ = super::logs::upload_chunk(
                        client,
                        config,
                        token,
                        job_id,
                        LogChunk {
                            chunk_index: (idx * 10000 + 9997) as i32,
                            step_order: idx as i32,
                            content: format!("==> ERROR <==\n{msg}\n"),
                        },
                    )
                    .await;
                    update_job_status(client, config, token, job_id, "failed", Some(idx as i32))
                        .await?;
                    anyhow::bail!(msg);
                }

                let cmd = command.to_string();
                let c = client.clone();

                // Debug echo — show the command before execution (like bash -x)
                {
                    let echo_line = build_step_echo(exec_type, &cmd, &step_env, &step_secret_keys, work_dir);
                    let _ = super::logs::upload_chunk(
                        client, config, token, job_id,
                        LogChunk { chunk_index: 0, step_order: idx as i32, content: echo_line },
                    ).await;
                }

                let cfg = config.clone();
                let cfg_ssh = cfg.clone();
                let t = token.to_string();
                let jid = job_id.to_string();
                let sname = step_name.to_string();
                let sname2 = sname.clone();
                let hosts_json = serde_json::Value::Array(host_values);

                let step_task = tokio::spawn(async move {
                    execute_ssh_step(&cfg_ssh, &cmd, &hosts_json, step_timeout, &sname, &step_env).await
                });
                let abort_handle = step_task.abort_handle();

                tokio::select! {
                    result = step_task => {
                        (result??, false)
                    }
                    _ = cancel_watcher(&c, &cfg, &t, &jid) => {
                        tracing::warn!("Job {jid} cancelled during SSH step '{sname2}' — aborting");
                        abort_handle.abort();
                        (engine::shell::ShellOutput {
                            stdout: String::new(),
                            stderr: format!("==> CANCELLED <==\n"),
                            exit_code: -1,
                        }, true)
                    }
                }
            }
            "docker" => {
                let image = step["image"].as_str().unwrap_or("").trim();
                if let Err(e) = engine::docker::preflight(config.engine.docker_enabled, image) {
                    let msg = e.to_string();
                    let _ = super::logs::upload_chunk(
                        client,
                        config,
                        token,
                        job_id,
                        LogChunk {
                            chunk_index: (idx * 10000 + 9997) as i32,
                            step_order: idx as i32,
                            content: format!("==> ERROR <==\n{msg}\n"),
                        },
                    )
                    .await;
                    update_job_status(client, config, token, job_id, "failed", Some(idx as i32))
                        .await?;
                    anyhow::bail!(msg);
                }

                let cmd = command.to_string();
                let volumes: Vec<String> = step
                    .get("volumes")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let privileged = step
                    .get("privileged")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                // Debug echo — show the command before execution (like bash -x)
                {
                    let echo_line = build_step_echo(exec_type, &cmd, &step_env, &step_secret_keys, work_dir);
                    let _ = super::logs::upload_chunk(
                        client, config, token, job_id,
                        LogChunk { chunk_index: 0, step_order: idx as i32, content: echo_line },
                    ).await;
                }

                let img = image.to_string();
                let env = step_env.clone();
                let c = client.clone();
                let cfg = config.clone();
                let t = token.to_string();
                let jid = job_id.to_string();
                let sname = step_name.to_string();
                let sv_for_upload = secret_values.clone();

                let (mut child, mut output_rx, read_guard) =
                    engine::docker::execute_streaming(
                        &img,
                        Path::new(&config.runner.workspace_dir),
                        &cmd,
                        &env,
                        &volumes,
                        work_dir,
                        privileged,
                    )
                    .await?;

                let c_log = client.clone();
                let cfg_log = config.clone();
                let t_log = token.to_string();
                let jid_log = job_id.to_string();
                let idx_i32 = idx as i32;
                let upload_task = tokio::spawn(async move {
                    let mut chunk_num = 1i32;
                    while let Some(chunk) = output_rx.recv().await {
                        let masked_chunk = crate::masker::mask_secrets(&chunk, &sv_for_upload);
                        match super::logs::upload_chunk(
                            &c_log,
                            &cfg_log,
                            &t_log,
                            &jid_log,
                            LogChunk {
                                chunk_index: chunk_num,
                                step_order: idx_i32,
                                content: masked_chunk,
                            },
                        )
                        .await
                        {
                            Ok(_) => {}
                            Err(e) => tracing::warn!("Log upload chunk {chunk_num} failed: {e}"),
                        }
                        chunk_num += 1;
                    }
                });

                let (exit_code, was_cancelled_docker, was_timeout_docker) = tokio::select! {
                    status = child.wait() => {
                        let code = status?.code().unwrap_or(-1);
                        let _ = read_guard.join().await;
                        let _ = upload_task.await;
                        (code, false, false)
                    }
                    _ = cancel_watcher(&c, &cfg, &t, &jid) => {
                        tracing::warn!("Job {jid} cancelled during docker step '{sname}' — killing container process");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        upload_task.abort();
                        (-1, true, false)
                    }
                    _ = tokio::time::sleep(step_timeout.unwrap_or(Duration::from_secs(3600))), if has_timeout => {
                        tracing::warn!("Docker step '{sname}' timed out after {step_timeout_secs}s — killing container");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        let _ = read_guard.join().await;
                        upload_task.abort();
                        (-1, false, true)
                    }
                };

                (
                    engine::shell::ShellOutput {
                        stdout: String::new(),
                        stderr: if was_cancelled_docker {
                            "==> CANCELLED <==\n".into()
                        } else if was_timeout_docker {
                            format!("==> TIMEOUT after {step_timeout_secs}s <==\n")
                        } else {
                            String::new()
                        },
                        exit_code: if was_cancelled_docker || was_timeout_docker { -1 } else { exit_code },
                    },
                    was_cancelled_docker,
                )
            }
            other => anyhow::bail!("unsupported engine type: {other}"),
        };
        // ─────────────────────────────────────────────────────────────────────

        if was_cancelled {
            let _ = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: (idx * 10000 + 9998) as i32,
                    step_order: idx as i32,
                    content: "==> CANCELLED <==\nJob was cancelled by user.\n".into(),
                },
            )
            .await;
            update_job_status(client, config, token, job_id, "cancelled", Some(idx as i32)).await?;
            anyhow::bail!("job cancelled by user");
        }

        // For SSH steps, upload the accumulated output as a summary chunk.
        // Local steps are fully streamed so we skip this redundant upload.
        if exec_type == "ssh" {
            let combined = format!("==> {step_name} <==\n{}\n{}", output.stdout, output.stderr);
            let masked = crate::masker::mask_secrets(&combined, &secret_values);
            super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: (idx * 10000 + 9999) as i32,
                    step_order: idx as i32,
                    content: masked,
                },
            )
            .await?;
        }

        if output.exit_code != 0 && fail_strategy == "stop" {
            update_job_status(client, config, token, job_id, "failed", Some(idx as i32)).await?;
            let trimmed_cmd = command.trim();
            let hint = if output.stderr.contains("TIMEOUT") {
                "the step exceeded its timeout.".to_string()
            } else {
                "the command may have tried to read interactive input. \
                 GIT_TERMINAL_PROMPT=0 is enabled — check that credentials are \
                 provided via secrets, or add the missing environment variables.".to_string()
            };
            anyhow::bail!(
                "step '{}' failed with exit code {}\n\
                 \x20  command: {}\n\
                 \x20  hint: {}",
                step_name, output.exit_code, trimmed_cmd, hint,
            );
        } else if output.exit_code != 0 && fail_strategy == "continue" {
            update_job_status(client, config, token, job_id, "failed", Some(idx as i32)).await?;
        } else {
            update_job_status(client, config, token, job_id, "success", Some(idx as i32)).await?;
        }
    }

    // All steps completed successfully
    update_job_status(client, config, token, job_id, "success", None).await?;
    tracing::info!("Job {job_id} completed successfully");

    Ok(())
}

/// Build an SSH command string with environment variable exports prepended.
/// Follows the same INPUT_* convention as the shell and docker engines,
/// and also exports step-level secret values.
fn build_ssh_env_command(
    command: &str,
    env: &std::collections::HashMap<String, String>,
) -> String {
    let mut prefix = String::from("set -e; ");
    if env.is_empty() {
        return format!("{prefix}{command}");
    }
    for (k, v) in env {
        // Export the raw variable
        let safe_v = shell_escape(v);
        prefix.push_str(&format!("export {k}={safe_v} && "));
        // Also export as INPUT_* for consistency (pipeline inputs convention)
        let input_key = format!("INPUT_{}", k.to_uppercase());
        prefix.push_str(&format!("export {input_key}={safe_v} && "));
    }
    format!("{}{}", prefix, command)
}

/// Minimal shell-escaping of values in single-quoted `export` statements.
fn shell_escape(value: &str) -> String {
    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Extracted SSH execution logic (keeps `execute_job` readable).
async fn execute_ssh_step(
    config: &Config,
    command: &str,
    host_values: &serde_json::Value,
    step_timeout: Option<std::time::Duration>,
    step_name: &str,
    env: &std::collections::HashMap<String, String>,
) -> anyhow::Result<engine::shell::ShellOutput> {
    let hosts = host_values
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("SSH step '{step_name}' host data is not an array"))?;

    let mut combined_stdout = String::new();
    let mut combined_stderr = String::new();
    let mut last_exit_code: i32 = 0;

    for (hi, hv) in hosts.iter().enumerate() {
        let host_label = hv["host"].as_str().unwrap_or("?");
        let host_params = hv
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("SSH step '{step_name}' host[{hi}] is not an object"))?;

        let ssh_conn = engine::ssh::SshConnectionParams {
            host: host_params
                .get("host")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            port: host_params
                .get("port")
                .and_then(|v| v.as_u64())
                .unwrap_or(22) as u16,
            username: host_params
                .get("username")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            credential_type: host_params
                .get("credential_type")
                .and_then(|v| v.as_str())
                .unwrap_or("key_path")
                .to_string(),
            credential_value: host_params
                .get("credential_value")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            credential_passphrase: host_params
                .get("credential_passphrase")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            host_key_policy: config.engine.ssh_host_key_policy.clone(),
            known_hosts_path: config.engine.ssh_known_hosts_path.clone(),
        };

        let ssh_header = if hosts.len() > 1 {
            format!("--- {step_name} @ {host_label} ---\n")
        } else {
            String::new()
        };

        let ssh_cmd = build_ssh_env_command(command, env);
        match engine::ssh::execute(&ssh_cmd, &ssh_conn, step_timeout).await {
            Ok(o) => {
                combined_stdout.push_str(&ssh_header);
                combined_stdout.push_str(&o.stdout);
                if !o.stderr.is_empty() {
                    if !combined_stdout.ends_with('\n') && !combined_stdout.is_empty() {
                        combined_stdout.push('\n');
                    }
                    combined_stderr.push_str(&ssh_header);
                    combined_stderr.push_str(&o.stderr);
                }
                last_exit_code = o.exit_code as i32;
                if o.exit_code != 0 {
                    tracing::warn!(
                        "SSH step '{step_name}' on {host_label} exited with code {}",
                        o.exit_code
                    );
                }
            }
            Err(e) => {
                tracing::error!("SSH step '{step_name}' on {host_label} failed: {e}");
                combined_stderr.push_str(&format!("{ssh_header}ERROR: {e}\n"));
                last_exit_code = -1;
            }
        }
    }

    Ok(engine::shell::ShellOutput {
        stdout: combined_stdout,
        stderr: combined_stderr,
        exit_code: last_exit_code,
    })
}

async fn update_job_status(
    client: &Client,
    config: &Config,
    token: &str,
    job_id: &str,
    status: &str,
    step_order: Option<i32>,
) -> anyhow::Result<()> {
    // Retry transient failures up to 3 times with short backoff.
    // Critical: if this fails silently the job stays stuck in "running" forever.
    let mut last_err = None;
    for attempt in 1..=3 {
        let resp = client
            .patch(&format!(
                "{}/api/v1/jobs/{job_id}/status",
                config.manager.url
            ))
            .header("Authorization", format!("Bearer {token}"))
            .json(&UpdateJobStatusRequest {
                status: status.to_string(),
                step_order,
            })
            .timeout(Duration::from_secs(15))
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) => {
                let code = r.status();
                let body = r.text().await.unwrap_or_default();
                last_err = Some(anyhow::anyhow!("status update returned {code}: {body}",));
            }
            Err(e) => {
                last_err = Some(super::friendly_reqwest_error(
                    e,
                    &config.manager.url,
                    "Job status update failed",
                ));
            }
        }

        if attempt < 3 {
            tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("status update failed after all retries")))
}

/// Clean the workspace directory by removing all contents.
/// The directory itself is preserved (recreated if needed) for future jobs.
fn clean_workspace(work_dir: &str) {
    match std::fs::remove_dir_all(work_dir) {
        Ok(()) => {
            if let Err(e) = std::fs::create_dir_all(work_dir) {
                tracing::warn!("Failed to recreate workspace dir '{work_dir}': {e}");
            }
        }
        Err(e) => {
            // If the directory doesn't exist, that's fine — nothing to clean
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("Failed to clean workspace '{work_dir}': {e}");
            }
        }
    }
}
