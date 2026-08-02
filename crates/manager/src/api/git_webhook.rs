//! GitHub / Gitee signed webhook → pipeline job trigger.

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use shared::error::AppError;
use shared::git_webhook::{
    branch_matches, has_skip_ci, normalize_provider, parse_gitee_event, parse_github_event,
    resolve_delivery_id, verify_gitee_token, verify_github_signature, NormalizedGitEvent,
    PROVIDER_GITEE, PROVIDER_GITHUB,
};
use shared::protocol::response::ApiResponse;
use sqlx::Row;

use super::trigger_events::{
    claim_delivery, finalize_event, insert_event, release_claim, ClaimResult, TriggerEventWrite,
};
use crate::crypto;
use crate::state::AppState;

/// POST /hooks/git/:provider/:hook_key
pub async fn handle(
    Path((provider_raw, hook_key)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<axum::response::Response, AppError> {
    let provider = normalize_provider(&provider_raw)
        .ok_or_else(|| AppError::NotFound(format!("unknown git provider '{provider_raw}'")))?;

    let row = sqlx::query(
        "SELECT h.pipeline_id, h.secret_ciphertext, h.branch_filter, h.enabled, h.provider, p.name, p.is_active
         FROM pipeline_git_hooks h
         JOIN pipelines p ON p.id = h.pipeline_id
         WHERE h.hook_key = ?",
    )
    .bind(&hook_key)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("no git hook for key '{hook_key}'")))?;

    let pipeline_id: String = row.get(0);
    let secret_ct: String = row.get(1);
    let branch_filter: String = row.get(2);
    let enabled: i64 = row.get(3);
    let configured_provider: String = row.get(4);
    let pipeline_name: String = row.get(5);
    let is_active: i64 = row.get(6);

    if configured_provider != provider {
        return Err(AppError::NotFound(format!(
            "hook is configured for '{configured_provider}', not '{provider}'"
        )));
    }
    if enabled == 0 {
        return Err(AppError::Conflict(format!(
            "git hook for pipeline '{pipeline_name}' is disabled"
        )));
    }
    if is_active == 0 {
        return Err(AppError::Conflict(format!(
            "pipeline '{pipeline_name}' is deactivated"
        )));
    }

    let secret = crypto::decrypt(&secret_ct, &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("git hook secret decrypt failed: {e}")))?;

    // Verify before parse side effects
    let verified = match provider {
        PROVIDER_GITHUB => {
            let sig = header_str(&headers, "x-hub-signature-256").unwrap_or_default();
            verify_github_signature(&secret, &body, &sig)
        }
        PROVIDER_GITEE => {
            let token = header_str(&headers, "x-gitee-token");
            verify_gitee_token(&secret, token.as_deref())
        }
        _ => false,
    };

    if !verified {
        let _ = insert_event(
            &state.pool,
            &TriggerEventWrite {
                pipeline_id: pipeline_id.clone(),
                source: "git".into(),
                provider: Some(provider.into()),
                event_type: "unknown".into(),
                decision: "rejected".into(),
                skip_reason: "bad_signature".into(),
                ..Default::default()
            },
        )
        .await;
        return Err(AppError::AuthFailed(
            "invalid git webhook signature/token".into(),
        ));
    }

    let body_json: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| AppError::ValidationFailed(format!("invalid JSON body: {e}")))?;

    let event: NormalizedGitEvent = match provider {
        PROVIDER_GITHUB => {
            let name = header_str(&headers, "x-github-event").unwrap_or_default();
            parse_github_event(&name, &body_json).map_err(AppError::ValidationFailed)?
        }
        PROVIDER_GITEE => {
            let name = header_str(&headers, "x-gitee-event").unwrap_or_default();
            parse_gitee_event(&name, &body_json).map_err(AppError::ValidationFailed)?
        }
        _ => return Err(AppError::NotFound("unknown provider".into())),
    };

    // GitHub: X-GitHub-Delivery. Gitee: never use X-Gitee-Timestamp — composite key.
    let github_delivery = header_str(&headers, "x-github-delivery");
    let delivery_id =
        resolve_delivery_id(provider, github_delivery.as_deref(), &event).unwrap_or_default();

    let claim_row = base_git_event(
        &pipeline_id,
        provider,
        &delivery_id,
        &event,
        "processing",
        "",
    );

    match claim_delivery(&state.pool, &claim_row).await? {
        ClaimResult::Existing {
            event_id,
            job_id,
            decision,
        } => {
            let status = if decision == "processing" {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            };
            Ok(json_status(
                status,
                serde_json::json!({
                    "decision": decision,
                    "event_id": event_id,
                    "job_id": job_id,
                    "pipeline_id": pipeline_id,
                    "pipeline_name": pipeline_name,
                    "idempotent": true,
                }),
            ))
        }
        ClaimResult::Claimed { event_id } => {
            return finish_claimed(
                &state,
                &event_id,
                &pipeline_id,
                &pipeline_name,
                provider,
                &branch_filter,
                &event,
            )
            .await;
        }
    }
}

async fn finish_claimed(
    state: &AppState,
    event_id: &str,
    pipeline_id: &str,
    pipeline_name: &str,
    provider: &str,
    branch_filter: &str,
    event: &NormalizedGitEvent,
) -> Result<axum::response::Response, AppError> {
    if event.is_ping {
        finalize_event(&state.pool, event_id, "skipped", "ping", None).await?;
        return Ok(json_status(
            StatusCode::OK,
            serde_json::json!({
                "decision": "skipped",
                "skip_reason": "ping",
                "event_id": event_id,
                "pipeline_id": pipeline_id,
                "pipeline_name": pipeline_name,
            }),
        ));
    }

    if has_skip_ci(&event.commit_message) {
        finalize_event(&state.pool, event_id, "skipped", "skip_ci", None).await?;
        return Ok(json_status(
            StatusCode::ACCEPTED,
            serde_json::json!({
                "decision": "skipped",
                "skip_reason": "skip_ci",
                "event_id": event_id,
                "pipeline_id": pipeline_id,
                "pipeline_name": pipeline_name,
            }),
        ));
    }

    if !event.branch.is_empty() && !branch_matches(branch_filter, &event.branch) {
        finalize_event(&state.pool, event_id, "skipped", "branch_mismatch", None).await?;
        return Ok(json_status(
            StatusCode::ACCEPTED,
            serde_json::json!({
                "decision": "skipped",
                "skip_reason": "branch_mismatch",
                "event_id": event_id,
                "branch": event.branch,
                "pipeline_id": pipeline_id,
                "pipeline_name": pipeline_name,
            }),
        ));
    }
    if event.branch.is_empty() && !branch_filter.trim().is_empty() && event.tag.is_empty() {
        finalize_event(&state.pool, event_id, "skipped", "branch_mismatch", None).await?;
        return Ok(json_status(
            StatusCode::ACCEPTED,
            serde_json::json!({
                "decision": "skipped",
                "skip_reason": "branch_mismatch",
                "event_id": event_id,
                "pipeline_id": pipeline_id,
                "pipeline_name": pipeline_name,
            }),
        ));
    }

    let inputs = event.to_inputs(provider);
    let trigger_result =
        super::pipelines::do_trigger(state, pipeline_id, "git", &inputs, false, false).await;

    match trigger_result {
        Ok(Json(resp)) => {
            let job_id = resp
                .data
                .as_ref()
                .and_then(|d| d.get("job_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            finalize_event(&state.pool, event_id, "triggered", "", job_id.as_deref()).await?;
            Ok(json_status(
                StatusCode::OK,
                serde_json::json!({
                    "decision": "triggered",
                    "event_id": event_id,
                    "job_id": job_id,
                    "pipeline_id": pipeline_id,
                    "pipeline_name": pipeline_name,
                    "status": resp.data.as_ref().and_then(|d| d.get("status")).cloned(),
                }),
            ))
        }
        Err(e) => {
            // Release claim so platform retries can create a job.
            if let Err(rel) = release_claim(&state.pool, event_id).await {
                tracing::warn!(event_id = %event_id, "failed to release webhook claim: {rel}");
            }
            tracing::warn!(
                pipeline = %pipeline_name,
                event_id = %event_id,
                "git webhook trigger failed: {e}"
            );
            Err(e)
        }
    }
}

fn base_git_event(
    pipeline_id: &str,
    provider: &str,
    delivery_id: &str,
    event: &NormalizedGitEvent,
    decision: &str,
    skip_reason: &str,
) -> TriggerEventWrite {
    TriggerEventWrite {
        pipeline_id: pipeline_id.to_string(),
        source: "git".into(),
        provider: Some(provider.into()),
        event_type: event.event_type.clone(),
        delivery_id: non_empty(delivery_id).map(|s| s.to_string()),
        repo_full_name: event.repo_full_name.clone(),
        git_ref: event.git_ref.clone(),
        branch: event.branch.clone(),
        tag: event.tag.clone(),
        commit_sha: event.commit_sha.clone(),
        commit_message: event.commit_message.clone(),
        sender: event.sender.clone(),
        compare_url: event.compare_url.clone(),
        decision: decision.into(),
        skip_reason: skip_reason.into(),
        job_id: None,
    }
}

fn non_empty(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

fn json_status(status: StatusCode, data: serde_json::Value) -> axum::response::Response {
    (status, Json(ApiResponse::success(data))).into_response()
}
