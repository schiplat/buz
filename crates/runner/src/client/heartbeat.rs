use reqwest::Client;
use shared::protocol::{HeartbeatRequest, HeartbeatResponse};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::config::Config;
use crate::policy::{self, SharedPolicy};

pub async fn run_heartbeat_loop(
    client: &Client,
    config: &Config,
    token: &str,
    started_at: std::time::Instant,
    shared_policy: SharedPolicy,
    restart_requested: Arc<AtomicBool>,
) {
    loop {
        // Refresh system info on every heartbeat so CPU/mem/disk stay current
        let system_info = crate::sysinfo::collect_system_info(
            started_at,
            &crate::sysinfo::SysInfoOpts {
                manager_url: config.runner.manager_url.clone(),
                prefer_interfaces: config.runner.prefer_interfaces.clone(),
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

                if prev.max_concurrent_jobs != next.max_concurrent_jobs
                    || prev.clean_workspace_after_job != next.clean_workspace_after_job
                    || prev.shell_enabled != next.shell_enabled
                    || prev.ssh_enabled != next.ssh_enabled
                    || prev.docker_enabled != next.docker_enabled
                    || prev.wasm_enabled != next.wasm_enabled
                    || prev.ssh_host_key_policy != next.ssh_host_key_policy
                {
                    tracing::info!(
                        "Runner policy updated from Manager: max_concurrent={}→{} \
                         clean={}→{} engines=[shell={},ssh={},docker={},wasm={}] ssh_host_key={}",
                        prev.max_concurrent_jobs,
                        next.max_concurrent_jobs,
                        prev.clean_workspace_after_job,
                        next.clean_workspace_after_job,
                        next.shell_enabled,
                        next.ssh_enabled,
                        next.docker_enabled,
                        next.wasm_enabled,
                        next.ssh_host_key_policy,
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
