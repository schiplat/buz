use shared::protocol::{RegisterRequest, RegisterResponse};
use reqwest::Client;

use crate::config::Config;

pub async fn register_with_center(client: &Client, config: &Config) -> anyhow::Result<String> {
    let req = RegisterRequest {
        name: config.node.name.clone(),
        register_token: config.manager.register_token.clone(),
        labels: config.node.labels.clone(),
        version: Some(config.node.version().into()),
    };

    let url = format!("{}/api/v1/runners/register", config.manager.url);
    let resp = client
        .post(&url)
        .json(&req)
        .send()
        .await
        .map_err(|e| super::friendly_reqwest_error(e, &config.manager.url, "Registration failed"))?;

    let status = resp.status();
    let body = resp.text().await?;
    tracing::debug!("register response raw body: {body}");

    if !status.is_success() {
        anyhow::bail!("registration failed: {body}");
    }

    // Parse the full ApiResponse envelope first, then extract data.
    let json: serde_json::Value = serde_json::from_str(&body)?;

    let data = json
        .get("data")
        .and_then(|d| serde_json::from_value::<RegisterResponse>(d.clone()).ok());

    let register_resp = data.ok_or_else(|| {
        anyhow::anyhow!(
            "empty or invalid data in registration response: {}",
            json.to_string()
        )
    })?;

    tracing::info!("Runner registered: id={}", register_resp.runner_id);

    Ok(register_resp.token)
}
