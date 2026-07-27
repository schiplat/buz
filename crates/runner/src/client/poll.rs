use shared::protocol::{LogChunk, UpdateJobStatusRequest};
use reqwest::Client;
use std::time::Duration;

use crate::config::Config;
use crate::engine;

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
        .map_err(|e| super::friendly_reqwest_error(e, &config.manager.url, "Status check failed"))?;

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
                        if let Err(se) = update_job_status(client, config, token, job_id, "cancelled", None).await {
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
                        .await {
                            tracing::warn!("Job {job_id}: failed to upload error log chunk: {le}");
                        }
                        if let Err(se) = update_job_status(client, config, token, job_id, "failed", None).await {
                            tracing::error!("CRITICAL: Failed to report failed status for job {job_id}: {se}");
                        }
                    }
                }
            }
            Ok(None) => {
                // No task — this is a normal poll, reset backoff
                if consecutive_failures > 0 {
                    tracing::info!(
                        "Poll recovered after {consecutive_failures} failure(s)"
                    );
                }
                consecutive_failures = 0;
            }
            Err(e) => {
                consecutive_failures += 1;

                let delay_secs = match consecutive_failures {
                    0 => 0, // unreachable in Err branch
                    n if n <= 5 => 2u64 << (n - 1),   // 2, 4, 8, 16, 32
                    _ => 60,                           // cap
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
            anyhow::bail!(
                "auth error ({status}) — token may have expired, re‑register needed"
            );
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

    // Build env map from trigger inputs
    let extra_env: std::collections::HashMap<String, String> = task
        .get("inputs")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    // ---- Cancel watcher helper -------------------------------------------------
    /// Returns `true` when the job has been cancelled.
    /// Polls the Manager every 2 s.
    async fn cancel_watcher(
        client: &Client,
        config: &Config,
        token: &str,
        job_id: &str,
    ) -> bool {
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
        let exec_type = step["exec_type"].as_str().unwrap_or("local");
        let fail_strategy = step["fail_strategy"].as_str().unwrap_or("stop");

        tracing::info!(
            "Step {idx}: {step_name} ({exec_type}){}",
            if extra_env.is_empty() { "" } else { " [with inputs]" }
        );

        update_job_status(client, config, token, job_id, "running", Some(idx as i32)).await?;

        // ── Race step execution against cancel polling ────────────────────────
        let (output, was_cancelled): (engine::local::LocalOutput, bool) = match exec_type {
            "local" => {
                let cmd = command.to_string();
                let wd = work_dir.map(|s| s.to_string());
                let env = extra_env.clone();
                let c = client.clone();
                let cfg = config.clone();
                let t = token.to_string();
                let jid = job_id.to_string();
                let sname = step_name.to_string();

                let (mut child, mut output_rx) =
                    engine::local::execute_streaming(&cmd, wd.as_deref(), Some(&env)).await?;

                // Background task: stream output chunks to the manager in real time
                let c_log = client.clone();
                let cfg_log = config.clone();
                let t_log = token.to_string();
                let jid_log = job_id.to_string();
                let idx_i32 = idx as i32;
                let upload_task = tokio::spawn(async move {
                    let mut chunk_num = 0i32;
                    while let Some(chunk) = output_rx.recv().await {
                        let _ = super::logs::upload_chunk(
                            &c_log, &cfg_log, &t_log, &jid_log,
                            LogChunk {
                                chunk_index: chunk_num,
                                step_order: idx_i32,
                                content: chunk,
                            },
                        ).await;
                        chunk_num += 1;
                    }
                });

                // Race: child completion vs cancellation
                let (exit_code, was_cancelled_local) = tokio::select! {
                    status = child.wait() => {
                        (status?.code().unwrap_or(-1), false)
                    }
                    _ = cancel_watcher(&c, &cfg, &t, &jid) => {
                        tracing::warn!("Job {jid} cancelled during step '{sname}' — killing");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        upload_task.abort();
                        (0, true)
                    }
                };

                // Flush trailing output
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), upload_task).await;

                (engine::local::LocalOutput {
                    stdout: String::new(),
                    stderr: if was_cancelled_local { "==> CANCELLED <==\n".into() } else { String::new() },
                    exit_code: if was_cancelled_local { -1 } else { exit_code },
                }, was_cancelled_local)
            }
            "ssh" => {
                let host_values = step["host"].as_array()
                    .map(|a| a.clone())
                    .or_else(|| step["host"].as_object().map(|o| vec![serde_json::Value::Object(o.clone())]))
                    .unwrap_or_else(|| {
                        let msg = format!("SSH step '{step_name}' missing host connection details");
                        // We can't await here since we're in a closure. Use a flag.
                        vec![]
                    });

                if host_values.is_empty() {
                    let msg = if step["host"].is_null() {
                        format!("SSH step '{step_name}' missing host connection details")
                    } else {
                        format!("SSH step '{step_name}': no hosts resolved from the database. Register your hosts in the Manager UI first.")
                    };
                    update_job_status(client, config, token, job_id, "failed", Some(idx as i32)).await?;
                    anyhow::bail!(msg);
                }

                let step_timeout = step["timeout_seconds"]
                    .as_i64()
                    .map(|s| std::time::Duration::from_secs(s as u64));

                let cmd = command.to_string();
                let c = client.clone();
                let cfg = config.clone();
                let t = token.to_string();
                let jid = job_id.to_string();
                let sname = step_name.to_string();
                let sname2 = sname.clone();
                let hosts_json = serde_json::Value::Array(host_values);

                let step_task = tokio::spawn(async move {
                    execute_ssh_step(&cmd, &hosts_json, step_timeout, &sname).await
                });
                let abort_handle = step_task.abort_handle();

                tokio::select! {
                    result = step_task => {
                        (result??, false)
                    }
                    _ = cancel_watcher(&c, &cfg, &t, &jid) => {
                        tracing::warn!("Job {jid} cancelled during SSH step '{sname2}' — aborting");
                        abort_handle.abort();
                        (engine::local::LocalOutput {
                            stdout: String::new(),
                            stderr: format!("==> CANCELLED <==\n"),
                            exit_code: -1,
                        }, true)
                    }
                }
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
                    chunk_index: idx as i32,
                    step_order: idx as i32,
                    content: "==> CANCELLED <==\nJob was cancelled by user.\n".into(),
                },
            )
            .await;
            update_job_status(client, config, token, job_id, "cancelled", Some(idx as i32)).await?;
            anyhow::bail!("job cancelled by user");
        }

        // Upload stdout as log chunk
        let combined = format!(
            "==> {step_name} <==\n{}\n{}",
            output.stdout, output.stderr
        );

        super::logs::upload_chunk(
            client,
            config,
            token,
            job_id,
            LogChunk {
                chunk_index: idx as i32,
                step_order: idx as i32,
                content: combined,
            },
        )
        .await?;

        if output.exit_code != 0 && fail_strategy == "stop" {
            update_job_status(client, config, token, job_id, "failed", Some(idx as i32)).await?;
            anyhow::bail!("step '{step_name}' failed with exit code {}", output.exit_code);
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

/// Extracted SSH execution logic (keeps `execute_job` readable).
async fn execute_ssh_step(
    command: &str,
    host_values: &serde_json::Value,
    step_timeout: Option<std::time::Duration>,
    step_name: &str,
) -> anyhow::Result<engine::local::LocalOutput> {
    let hosts = host_values.as_array()
        .ok_or_else(|| anyhow::anyhow!("SSH step '{step_name}' host data is not an array"))?;

    let mut combined_stdout = String::new();
    let mut combined_stderr = String::new();
    let mut last_exit_code: i32 = 0;

    for (hi, hv) in hosts.iter().enumerate() {
        let host_label = hv["host"].as_str().unwrap_or("?");
        let host_params = hv.as_object()
            .ok_or_else(|| anyhow::anyhow!("SSH step '{step_name}' host[{hi}] is not an object"))?;

        let ssh_conn = engine::ssh::SshConnectionParams {
            host: host_params["host"].as_str().unwrap_or("").to_string(),
            port: host_params["port"].as_u64().unwrap_or(22) as u16,
            username: host_params["username"].as_str().unwrap_or("").to_string(),
            credential_type: host_params["credential_type"].as_str().unwrap_or("key_path").to_string(),
            credential_ref: host_params["credential_ref"].as_str().unwrap_or("").to_string(),
        };

        let ssh_header = if hosts.len() > 1 {
            format!("--- {step_name} @ {host_label} ---\n")
        } else {
            String::new()
        };

        match engine::ssh::execute(command, &ssh_conn, step_timeout).await {
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
                    tracing::warn!("SSH step '{step_name}' on {host_label} exited with code {}", o.exit_code);
                }
            }
            Err(e) => {
                tracing::error!("SSH step '{step_name}' on {host_label} failed: {e}");
                combined_stderr.push_str(&format!("{ssh_header}ERROR: {e}\n"));
                last_exit_code = -1;
            }
        }
    }

    Ok(engine::local::LocalOutput {
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
            .patch(&format!("{}/api/v1/jobs/{job_id}/status", config.manager.url))
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
                last_err = Some(anyhow::anyhow!(
                    "status update returned {code}: {body}",
                ));
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
