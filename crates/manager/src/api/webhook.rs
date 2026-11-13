use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use shared::error::AppError;
use shared::git_webhook::verify_hex_hmac_sha256;
use shared::protocol::response::ApiResponse;

use crate::state::AppState;

/// Query params accepted by the webhook endpoint. `environment` is reserved for
/// deployment targeting; other parameters are passed as pipeline inputs.
#[derive(serde::Deserialize, Default)]
pub struct HookQuery {
    /// Input values passed as query parameters, e.g. ?version=1.0&env=staging
    #[serde(flatten)]
    pub inputs: std::collections::HashMap<String, String>,
}

/// Accept webhook triggers. Supports:
/// - Query params:     POST /api/v1/hooks/deploy-prod?version=1.2&environment=staging
/// - JSON body:        POST /api/v1/hooks/deploy-prod {"environment":"staging","inputs":{"version":"1.2"}}
/// - Both (query selector wins over body selector; body inputs override query inputs)
///
/// The pipeline MUST declare `on.webhook.secret: <NAME>` — an unprotected
/// webhook would be triggerable by anyone who can guess the path. Requests
/// carry `X-Buz-Signature: sha256=<hex HMAC-SHA256 of the raw body>`; the
/// secret value is resolved from the pipeline's team at trigger time (only
/// the name is stored).
pub async fn handle(
    Path(webhook_path): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<HookQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    // Look up pipeline by webhook_path (case-sensitive exact match)
    let pipeline = crate::store::webhook_pipeline::find_by_path(&state.pool, &webhook_path).await?;
    let pipeline_id = pipeline.id.clone();
    let pipeline_name = pipeline.name.clone();
    let webhook_secret_name = pipeline.webhook_secret_name.clone();
    let team_id = pipeline.team_id.clone();
    let configured_environment = pipeline.webhook_environment.clone();
    let mut environment = query.inputs.get("environment").cloned();

    if !pipeline.active {
        return Err(AppError::Conflict(format!(
            "pipeline '{pipeline_name}' is deactivated"
        )));
    }

    // Signature gate — verified before the body is parsed or acted on.
    // A webhook without a secret is triggerable by anyone who can guess the
    // path, so configuring `on.webhook.secret` is mandatory.
    if webhook_secret_name.is_empty() {
        tracing::warn!(
            "Webhook rejected: pipeline '{pipeline_name}' has no on.webhook.secret configured"
        );
        return Err(AppError::Forbidden(
            "this webhook is unprotected — set `on.webhook.secret: <secret name>` on the pipeline, then sign requests with X-Buz-Signature"
                .into(),
        ));
    }
    if !webhook_secret_name.is_empty() {
        let ciphertext: Option<String> = crate::store::webhook_pipeline::team_secret_value(
            &state.pool,
            &team_id,
            &webhook_secret_name,
        )
        .await?;
        let secret_value = match ciphertext
            .and_then(|c| crate::crypto::decrypt(&c, &state.config.auth.jwt_secret).ok())
        {
            Some(v) => v,
            None => {
                tracing::warn!(
                    "Webhook rejected: secret '{webhook_secret_name}' missing or undecryptable \
                     for pipeline '{pipeline_name}'"
                );
                return Err(AppError::Forbidden(
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
                return Err(AppError::Forbidden(
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

    let mut inputs = merge_hook_inputs(&query.inputs, &body_json);
    if environment.is_none() {
        environment = body_json
            .get("environment")
            .or_else(|| body_json.get("inputs").and_then(|v| v.get("environment")))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
    }
    if environment.is_none() && !configured_environment.is_empty() {
        environment = Some(configured_environment);
    }
    inputs.remove("environment");

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
        environment.as_deref(),
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
    inputs.remove("environment");

    // JSON body "inputs" object overrides query params. The environment
    // selector is handled separately and never enters business inputs.
    if let Some(body_inputs) = body.get("inputs").and_then(|v| v.as_object()) {
        for (k, v) in body_inputs {
            if k != "environment" {
                if let Some(s) = v.as_str() {
                    inputs.insert(k.clone(), s.to_string());
                }
            }
        }
    }

    // Flat JSON body keys (GitHub/CI style) — only set if not already present
    if let Some(obj) = body.as_object() {
        for (k, v) in obj {
            if k != "inputs" && k != "environment" {
                if let Some(s) = v.as_str() {
                    inputs.entry(k.clone()).or_insert_with(|| s.to_string());
                }
            }
        }
    }

    inputs
}
