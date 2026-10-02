use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use shared::error::AppError;
use shared::git_webhook::verify_hex_hmac_sha256;
use shared::protocol::response::ApiResponse;
use sqlx::Row;

use crate::state::AppState;

/// Query params accepted by the webhook endpoint.
#[derive(serde::Deserialize, Default)]
pub struct HookQuery {
    /// Input values passed as query parameters, e.g. ?version=1.0&env=staging
    #[serde(flatten)]
    pub inputs: std::collections::HashMap<String, String>,
}

/// Accept webhook triggers. Supports:
/// - Query params:     POST /api/v1/hooks/deploy-prod?version=1.2&env=staging
/// - JSON body inputs: POST /api/v1/hooks/deploy-prod {"inputs":{"version":"1.2"}}
/// - Both (JSON wins for overlapping keys)
///
/// When the pipeline declares `on.webhook.secret: <NAME>`, requests must also carry
/// `X-Buz-Signature: sha256=<hex HMAC-SHA256 of the raw body>`. The secret value is
/// resolved from the pipeline's team at trigger time (only the name is stored).
pub async fn handle(
    Path(webhook_path): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<HookQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    // Look up pipeline by webhook_path (case-sensitive exact match)
    let pipeline = sqlx::query(
        "SELECT id, name, is_active, input_defs, webhook_secret, team_id \
         FROM pipelines WHERE trigger_type = 'webhook' AND webhook_path = ?",
    )
    .bind(&webhook_path)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| {
        AppError::NotFound(format!(
            "no webhook pipeline found for path '/{webhook_path}'"
        ))
    })?;

    let pipeline_id: String = pipeline.get(0);
    let pipeline_name: String = pipeline.get(1);
    let is_active: i64 = pipeline.get(2);
    let webhook_secret_name: String = pipeline.get(4);
    let team_id: String = pipeline.get(5);

    if is_active == 0 {
        return Err(AppError::Conflict(format!(
            "pipeline '{pipeline_name}' is deactivated"
        )));
    }

    // Signature gate — verified before the body is parsed or acted on.
    if !webhook_secret_name.is_empty() {
        let ciphertext: Option<String> =
            sqlx::query("SELECT value FROM secrets WHERE team_id = ? AND name = ?")
                .bind(&team_id)
                .bind(&webhook_secret_name)
                .fetch_optional(&state.pool)
                .await?
                .map(|r| r.get(0));
        let secret_value = match ciphertext
            .and_then(|c| crate::crypto::decrypt(&c, &state.config.auth.jwt_secret).ok())
        {
            Some(v) => v,
            None => {
                tracing::warn!(
                    "Webhook rejected: secret '{webhook_secret_name}' missing or undecryptable \
                     for pipeline '{pipeline_name}'"
                );
                return Err(AppError::Unauthorized(
                    "webhook secret is configured on this pipeline but could not be resolved \
                     — ask an admin to check the team secret"
                        .into(),
                ));
            }
        };
        let provided = headers
            .get("x-buz-signature")
            .and_then(|v| v.to_str().ok())
            .map(str::trim);
        match provided {
            Some(sig) if verify_hex_hmac_sha256(&secret_value, &body, sig) => {}
            _ => {
                tracing::warn!(
                    "Webhook rejected: invalid or missing X-Buz-Signature for pipeline '{pipeline_name}'"
                );
                return Err(AppError::Unauthorized(
                    "invalid or missing X-Buz-Signature header".into(),
                ));
            }
        }
    }

    let body_json: serde_json::Value = if body.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| AppError::ValidationFailed(format!("invalid JSON body: {e}")))?
    };

    let inputs = merge_hook_inputs(&query.inputs, &body_json);

    tracing::info!(
        "Webhook trigger: pipeline={pipeline_name} path=/{webhook_path} inputs={:?}",
        inputs
    );

    super::pipelines::do_trigger(
        &state,
        &pipeline_id,
        "webhook",
        &inputs,
        false,
        false,
        None,
        None,
        None,
    )
    .await
}

/// Merge query params with JSON body. Body keys override query params.
/// Also accepts flat JSON body keys (e.g. `{"version": "1.0"}`) as inputs.
pub fn merge_hook_inputs(
    query_inputs: &std::collections::HashMap<String, String>,
    body: &serde_json::Value,
) -> std::collections::HashMap<String, String> {
    let mut inputs = query_inputs.clone();

    // JSON body "inputs" object overrides query params
    if let Some(body_inputs) = body.get("inputs").and_then(|v| v.as_object()) {
        for (k, v) in body_inputs {
            if let Some(s) = v.as_str() {
                inputs.insert(k.clone(), s.to_string());
            }
        }
    }

    // Flat JSON body keys (GitHub/CI style) — only set if not already present
    if let Some(obj) = body.as_object() {
        for (k, v) in obj {
            if k != "inputs" {
                if let Some(s) = v.as_str() {
                    inputs.entry(k.clone()).or_insert_with(|| s.to_string());
                }
            }
        }
    }

    inputs
}

#[cfg(test)]
#[path = "tests/webhook_tests.rs"]
mod tests;
