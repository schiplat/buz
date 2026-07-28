use reqwest::Client;
use shared::protocol::HeartbeatRequest;

use crate::config::Config;

pub async fn run_heartbeat_loop(
    client: &Client,
    config: &Config,
    token: &str,
    started_at: std::time::Instant,
) {
    loop {
        // Refresh system info on every heartbeat so CPU/mem/disk stay current
        let system_info = crate::sysinfo::collect_system_info(
            started_at,
            &crate::sysinfo::SysInfoOpts {
                manager_url: config.manager.url.clone(),
                prefer_interfaces: config.runner.prefer_interfaces.clone(),
            },
        );

        match send_heartbeat(client, config, token, &system_info).await {
            Ok(()) => tracing::debug!("Heartbeat sent"),
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
) -> anyhow::Result<()> {
    let url = format!("{}/api/v1/runners/heartbeat", config.manager.url);
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .json(&HeartbeatRequest {
            status: "online".into(),
            system_info: Some(system_info.clone()),
        })
        .send()
        .await
        .map_err(|e| super::friendly_reqwest_error(e, &config.manager.url, "Heartbeat failed"))?;

    if !resp.status().is_success() {
        anyhow::bail!("heartbeat returned {}", resp.status());
    }

    Ok(())
}
