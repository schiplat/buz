use reqwest::Client;
use shared::protocol::{HeartbeatRequest, HeartbeatResponse};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
) {
    let mut capabilities = probe_execution_capabilities(config).await;
    // Host-verified netns support probed alongside the tier capabilities:
    // `unshare --net` is Linux-only and needs privileges, so the UI can gate
    // its policy toggle instead of letting jobs fail at preflight.
    let mut netns_supported = crate::engine::shell::probe_netns_supported().await;
    let mut last_capability_check = std::time::Instant::now();
    // Set when the Manager serves a one-shot re-probe command: probe at once
    // and report fresh results on the very next heartbeat instead of idling.
    let mut reprobe_requested = false;
    // Observes sandboxd's applied configuration every heartbeat (cheap
    // `status`), maintains the observational snapshot, and tells us when
    // the config version changed so capabilities are re-probed at once
    // instead of on the 5-minute cadence.
    let mut observer = crate::client::sandbox_snapshot::Observer::default();
    loop {
        if reprobe_requested {
            tracing::info!("Capability re-probe requested by Manager — probing now");
            capabilities = probe_execution_capabilities(config).await;
            netns_supported = crate::engine::shell::probe_netns_supported().await;
            last_capability_check = std::time::Instant::now();
            reprobe_requested = false;
        }
        let sandboxd_block = if policy::snapshot(&shared_policy)
            .execution_mode
            .is_sandboxed()
        {
            let observation = observer.observe(config).await;
            if observation.config_changed {
                tracing::info!("sandboxd configuration changed; re-probing capabilities");
                capabilities = probe_execution_capabilities(config).await;
                last_capability_check = std::time::Instant::now();
            }
            Some(observation.block)
        } else {
            None
        };
        if last_capability_check.elapsed() >= std::time::Duration::from_secs(300) {
            capabilities = probe_execution_capabilities(config).await;
            netns_supported = crate::engine::shell::probe_netns_supported().await;
            last_capability_check = std::time::Instant::now();
        }
        // Refresh system info on every heartbeat so CPU/mem/disk stay current.
        let system_info = crate::sysinfo::collect_system_info(
            started_at,
            &crate::sysinfo::SysInfoOpts {
                manager_url: config.runner.manager_url.clone(),
                prefer_interfaces: config.runner.prefer_interfaces.clone(),
                execution_capabilities: Some(capabilities.clone()),
                netns_supported: Some(netns_supported),
                sandboxd: sandboxd_block,
            },
        );

        match send_heartbeat(client, config, token, &system_info).await {
            Ok(resp) => {
                let next = resp.policy;
                let prev = policy::store(&shared_policy, next.clone());

                if resp.restart {
                    tracing::info!("Restart command received from Manager — signaling poll loop");
                    restart_requested.store(true, Ordering::SeqCst);
                }

                if resp.reprobe {
                    reprobe_requested = true;
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

        // A just-served re-probe loops straight back: the top of the loop
        // probes immediately and the fresh report goes out right away.
        if reprobe_requested {
            continue;
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
) -> anyhow::Result<HeartbeatResponse> {
    let url = format!("{}/api/v1/runners/heartbeat", config.runner.manager_url);
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
