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

use super::trigger_events::{
    claim_delivery, finalize_event, insert_event, release_claim, ClaimResult, TriggerEventWrite,
};
use crate::crypto;
use crate::state::AppState;
use crate::store::git_hooks::find_by_hook_key;

/// POST /hooks/git/:provider/:hook_key
pub async fn handle(
    Path((provider_raw, hook_key)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<axum::response::Response, AppError> {
    let provider = normalize_provider(&provider_raw)
        .ok_or_else(|| AppError::NotFound(format!("unknown git provider '{provider_raw}'")))?;

    let hook = find_by_hook_key(&state.pool, &hook_key).await?;

    let pipeline_id = hook.pipeline_id;
    let branch_filter = hook.branch_filter;
    let pipeline_name = hook.pipeline_name;
    let event_filter = hook.event_filter;
    let environment = hook.environment;

    if hook.provider != provider {
        return Err(AppError::NotFound(format!(
            "hook is configured for '{}', not '{provider}'",
            hook.provider
        )));
    }
    if !hook.enabled {
        return Err(AppError::Conflict(format!(
            "git hook for pipeline '{pipeline_name}' is disabled"
        )));
    }
    if !hook.pipeline_active {
        return Err(AppError::Conflict(format!(
            "pipeline '{pipeline_name}' is deactivated"
        )));
    }

    let secret = crypto::decrypt(&hook.secret_ciphertext, &state.config.auth.jwt_secret)
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
                &event_filter,
                &event,
                &environment,
            )
            .await;
        }
    }
}

#[allow(clippy::too_many_arguments)] // handler context: state, path, body, headers, auth
async fn finish_claimed(
    state: &AppState,
    event_id: &str,
    pipeline_id: &str,
    pipeline_name: &str,
    provider: &str,
    branch_filter: &str,
    event_filter: &str,
    event: &NormalizedGitEvent,
    environment: &str,
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

    // Parsed-but-not-actionable deliveries (PR closed/labeled, draft PR,
    // merged MR) are recorded and acknowledged — returning an error would
    // make the provider retry them.
    if !event.ignored_reason.is_empty() {
        let reason = event.ignored_reason.clone();
        finalize_event(&state.pool, event_id, "skipped", &reason, None).await?;
        return Ok(json_status(
            StatusCode::OK,
            serde_json::json!({
                "decision": "skipped",
                "skip_reason": reason,
                "event_id": event_id,
                "event": event.event_type,
                "pipeline_id": pipeline_id,
                "pipeline_name": pipeline_name,
            }),
        ));
    }

    // Event subscription: hooks default to `push`; pull-request deliveries
    // are opt-in per hook (`event_filter`), so existing hooks never start
    // firing on PRs after an upgrade.
    if !event_filter_allows(event_filter, &event.event_type) {
        finalize_event(
            &state.pool,
            event_id,
            "skipped",
            "event_not_subscribed",
            None,
        )
        .await?;
        return Ok(json_status(
            StatusCode::OK,
            serde_json::json!({
                "decision": "skipped",
                "skip_reason": "event_not_subscribed",
                "event_id": event_id,
                "event": event.event_type,
                "event_filter": event_filter,
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
    let trigger_result = super::pipelines::do_trigger(
        state,
        pipeline_id,
        "git",
        &inputs,
        false,
        false,
        Some(environment),
        None,
        None,
    )
    .await;

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

/// Whether a hook's comma-separated `event_filter` accepts an event type.
/// Empty/legacy rows mean push only (the pre-PR behavior).
pub(crate) fn event_filter_allows(filter: &str, event_type: &str) -> bool {
    let filter = filter.trim();
    if filter.is_empty() {
        return event_type == "push";
    }
    filter
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .any(|part| part == event_type)
}

/// Validate an `event_filter` from the API and return its stored form
/// (`push`, `push,pull_request`, …). Defaults to `push`.
pub(crate) fn normalize_event_filter(raw: Option<&str>) -> Result<String, AppError> {
    const ALLOWED: &[&str] = &["push", "pull_request"];
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok("push".into());
    };
    let mut selected: Vec<&str> = Vec::new();
    for part in raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if !ALLOWED.contains(&part) {
            return Err(AppError::ValidationFailed(format!(
                "event_filter '{part}' is not supported — use push or pull_request"
            )));
        }
        if !selected.contains(&part) {
            selected.push(part);
        }
    }
    if selected.is_empty() {
        return Ok("push".into());
    }
    Ok(selected.join(","))
}

#[cfg(test)]
mod event_filter_tests {
    use super::*;

    #[test]
    fn legacy_empty_filter_is_push_only() {
        assert!(event_filter_allows("", "push"));
        assert!(!event_filter_allows("", "pull_request"));
    }

    #[test]
    fn filter_lists_accept_subscribed_events_only() {
        assert!(event_filter_allows("push,pull_request", "pull_request"));
        assert!(event_filter_allows("push", "push"));
        assert!(!event_filter_allows("pull_request", "push"));
        assert!(!event_filter_allows("push", "ping"));
    }

    #[test]
    fn normalize_defaults_to_push_and_deduplicates() {
        assert_eq!(normalize_event_filter(None).unwrap(), "push");
        assert_eq!(normalize_event_filter(Some("  ")).unwrap(), "push");
        assert_eq!(
            normalize_event_filter(Some("pull_request,push,pull_request")).unwrap(),
            "pull_request,push"
        );
    }

    #[test]
    fn normalize_rejects_unknown_events() {
        let error = normalize_event_filter(Some("push,tag")).unwrap_err();
        assert!(error.to_string().contains("not supported"));
    }
}
