use reqwest::Client;
use shared::protocol::{RegisterRequest, RegisterResponse, RunnerPolicyConfig};

use crate::config::Config;

pub struct RegisterResult {
    pub token: String,
    pub policy: RunnerPolicyConfig,
}

pub async fn register_with_center(client: &Client, config: &Config) -> anyhow::Result<RegisterResult> {
    let req = RegisterRequest {
        name: config.runner.name.clone(),
        register_token: config.manager.register_token.clone(),
        // Labels are configured in Manager UI; agent must not overwrite them.
        labels: Vec::new(),
        version: Some(config.runner.version().into()),
    };

    let url = format!("{}/api/v1/runners/register", config.manager.url);
    let resp = client.post(&url).json(&req).send().await.map_err(|e| {
        super::friendly_reqwest_error(e, &config.manager.url, "Registration failed")
    })?;

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

    let mut policy = register_resp.policy;
    policy.max_concurrent_jobs = policy.max_concurrent_jobs.max(1);

    tracing::info!(
        "Runner registered: id={} max_concurrent={} ephemeral={} engines=[shell={},ssh={},docker={},wasm={}]",
        register_resp.runner_id,
        policy.max_concurrent_jobs,
        policy.ephemeral,
        policy.shell_enabled,
        policy.ssh_enabled,
        policy.docker_enabled,
        policy.wasm_enabled,
    );

    Ok(RegisterResult {
        token: register_resp.token,
        policy,
    })
}
