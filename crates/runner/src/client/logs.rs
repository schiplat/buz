use reqwest::Client;
use shared::protocol::LogChunk;
use std::time::Duration;

use crate::config::Config;

pub async fn upload_chunk(
    client: &Client,
    config: &Config,
    token: &str,
    job_id: &str,
    chunk: LogChunk,
) -> anyhow::Result<()> {
    let resp = tokio::time::timeout(Duration::from_secs(30), async {
        client
            .post(&format!("{}/api/v1/jobs/{job_id}/logs", config.manager.url))
            .header("Authorization", format!("Bearer {token}"))
            .json(&chunk)
            .send()
            .await
    })
    .await
    .map_err(|_| anyhow::anyhow!("log upload timed out after 30s"))?
    .map_err(|e| super::friendly_reqwest_error(e, &config.manager.url, "Log upload failed"))?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "log upload returned {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }

    Ok(())
}
