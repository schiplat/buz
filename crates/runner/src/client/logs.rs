use shared::protocol::LogChunk;
use reqwest::Client;

use crate::config::Config;

pub async fn upload_chunk(
    client: &Client,
    config: &Config,
    token: &str,
    job_id: &str,
    chunk: LogChunk,
) -> anyhow::Result<()> {
    let resp = client
        .post(&format!("{}/api/v1/jobs/{job_id}/logs", config.manager.url))
        .header("Authorization", format!("Bearer {token}"))
        .json(&chunk)
        .send()
        .await
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
