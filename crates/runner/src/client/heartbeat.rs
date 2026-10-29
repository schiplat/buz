use reqwest::Client;
use shared::protocol::{HeartbeatRequest, HeartbeatResponse};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::client::clock::ClockMonitor;
use crate::config::Config;
use crate::policy::{self, SharedPolicy};
use shared::protocol::RunnerExecutionCapabilities;

pub async fn run_heartbeat_loop(
    client: &Client,
    config: &Config,
    token: &str,
    started_at: std::time::Instant,
    shared_policy: SharedPolicy,
    restart_requested: Arc<AtomicBool>,
    clock_monitor: Arc<ClockMonitor>,
) {
    let mut capabilities = probe_execution_capabilities(config).await;
    let mut last_capability_check = std::time::Instant::now();
    loop {
        if last_capability_check.elapsed() >= std::time::Duration::from_secs(300) {
            capabilities = probe_execution_capabilities(config).await;
            last_capability_check = std::time::Instant::now();
        }
        // Refresh system info on every heartbeat so CPU/mem/disk stay current;
        // the clock skew measured on the previous heartbeat rides along so the
        // Manager UI can show how far this Runner drifts.
        let system_info = crate::sysinfo::collect_system_info(
            started_at,
            &crate::sysinfo::SysInfoOpts {
                manager_url: config.runner.manager_url.clone(),
                prefer_interfaces: config.runner.prefer_interfaces.clone(),
                clock_skew_seconds: clock_monitor.skew_secs(),
                execution_capabilities: Some(capabilities.clone()),
            },
        );

        match send_heartbeat(client, config, token, &system_info, &clock_monitor).await {
            Ok(resp) => {
                let next = resp.policy;
                let prev = policy::store(&shared_policy, next.clone());

                if resp.restart {
                    tracing::info!("Restart command received from Manager — signaling poll loop");
                    restart_requested.store(true, Ordering::SeqCst);
                }

                if prev != next {
                    tracing::info!(
                        "Runner policy updated from Manager: max_concurrent={}→{} \
                         clean={}→{} engines=[shell={}→{},ssh={}→{},docker={}→{},wasm={}→{}] \
                         ssh_host_key={}→{} mode={}",
                        prev.max_concurrent_jobs,
                        next.max_concurrent_jobs,
                        prev.clean_workspace_after_job,
                        next.clean_workspace_after_job,
                        prev.shell_enabled,
                        next.shell_enabled,
                        prev.ssh_enabled,
                        next.ssh_enabled,
                        prev.docker_enabled,
                        next.docker_enabled,
                        prev.wasm_enabled,
                        next.wasm_enabled,
                        prev.ssh_host_key_policy,
                        next.ssh_host_key_policy,
                        next.execution_mode.as_str(),
                    );
                } else {
                    tracing::debug!(
                        "Heartbeat sent (max_concurrent={})",
                        next.max_concurrent_jobs
                    );
                }
            }
            Err(e) => tracing::warn!("Heartbeat failed: {e}"),
        }

        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

async fn probe_execution_capabilities(config: &Config) -> RunnerExecutionCapabilities {
    match crate::client::sandbox_client::probe_capabilities(&config.sandbox).await {
        Ok(capabilities) => capabilities,
        Err(error) => {
            // Supervisor unreachable: every sandboxed tier is unavailable, and the
            // Manager must see the same reason for each of them.
            let reason = format!("sandbox supervisor probe failed: {error:#}");
            let mut capabilities = RunnerExecutionCapabilities::default();
            for mode in shared::protocol::RunnerExecutionMode::ALL
                .into_iter()
                .filter(|mode| mode.is_sandboxed())
            {
                capabilities.set_backend(
                    mode,
                    shared::protocol::ExecutionBackendStatus::Unsupported,
                    Some(reason.clone()),
                );
            }
            capabilities.checked_at = Some(chrono::Utc::now().to_rfc3339());
            capabilities
        }
    }
}

async fn send_heartbeat(
    client: &Client,
    config: &Config,
    token: &str,
    system_info: &shared::protocol::SystemInfo,
    clock_monitor: &ClockMonitor,
) -> anyhow::Result<HeartbeatResponse> {
    let url = format!("{}/api/v1/runners/heartbeat", config.runner.manager_url);
    let sent_at = chrono::Utc::now();
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .json(&HeartbeatRequest {
            status: "online".into(),
            system_info: Some(system_info.clone()),
            host_id: config.runner.host_id.clone(),
            runner_instance_id: config.runner.instance_id.clone(),
        })
        .send()
        .await
        .map_err(|e| {
            super::friendly_reqwest_error(e, &config.runner.manager_url, "Heartbeat failed")
        })?;

    // Free clock re-check: the Manager's `Date` header rides on every response.
    clock_monitor.observe(sent_at, resp.headers());

    if !resp.status().is_success() {
        anyhow::bail!("heartbeat returned {}", resp.status());
    }

    let body = resp.json::<serde_json::Value>().await?;
    let mut resp = body
        .get("data")
        .and_then(|d| serde_json::from_value::<HeartbeatResponse>(d.clone()).ok())
        .unwrap_or_default();
    resp.policy.max_concurrent_jobs = resp.policy.max_concurrent_jobs.max(1);
    Ok(resp)
}
