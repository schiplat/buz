//! Admin management for step sources and the template pool
//! (step-marketplace §6.8 Phase 1).

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use uuid::Uuid;

use crate::middleware::PlatformAdmin;
use crate::services::step_pool;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateSourceRequest {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub auto_enable: bool,
}

#[derive(Deserialize)]
pub struct TogglePoolRequest {
    pub enabled: bool,
}

fn http_client() -> reqwest::Client {
    reqwest::Client::new()
}

/// GET /api/v1/admin/step-sources — all configured sources with sync state.
pub async fn list_sources(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let items = step_pool::list_sources(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::success(json!({ "items": items }))))
}

/// POST /api/v1/admin/step-sources — add a source and sync it immediately.
/// A failed sync keeps the source registered (last_sync_error explains);
/// rows only land in the pool once an index is valid.
pub async fn create_source(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(body): Json<CreateSourceRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let name = body.name.trim().to_string();
    let url = body.url.trim().to_string();
    if name.is_empty() {
        return Err(AppError::ValidationFailed("name must not be empty".into()));
    }
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(AppError::ValidationFailed(
            "url must be an absolute http(s) index.json URL".into(),
        ));
    }

    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO step_sources (id, name, url, auto_enable, enabled, created_by)
         VALUES (?, ?, ?, ?, 1, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&url)
    .bind(body.auto_enable as i64)
    .bind(&auth.username)
    .execute(&state.pool)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    let source = step_pool::get_source(&state.pool, &id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or(AppError::NotFound("source disappeared after insert".into()))?;
    let http = http_client();
    let synced = step_pool::sync_source(&http, &state.pool, &source)
        .await
        .is_ok();

    let source = step_pool::get_source(&state.pool, &id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::success(json!({
        "source": source,
        "synced": synced,
    }))))
}

/// POST /api/v1/admin/step-sources/{id}/sync — re-fetch the index.
pub async fn sync_source(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let source = step_pool::get_source(&state.pool, &id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or(AppError::NotFound("source not found".into()))?;
    let http = http_client();
    let result = step_pool::sync_source(&http, &state.pool, &source).await;
    match result {
        Ok(count) => Ok(Json(ApiResponse::success(json!({
            "id": id, "synced": true, "entries": count,
        })))),
        Err(e) => Ok(Json(ApiResponse::success(json!({
            "id": id, "synced": false, "error": e.to_string(),
        })))),
    }
}

/// DELETE /api/v1/admin/step-sources/{id} — removes the source and its pool
/// rows. Already-saved pipelines are unaffected (resolution froze at save).
pub async fn delete_source(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    sqlx::query("DELETE FROM step_sources WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::success(json!({ "id": id }))))
}

/// GET /api/v1/admin/step-pool — every pool row (enabled + staged).
pub async fn list_pool(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let items = step_pool::list_pool(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::success(json!({ "items": items }))))
}

/// PUT /api/v1/admin/step-pool/{namespace}/{step_id}/{version} — enable or
/// disable one pool entry (the curator's curation action).
pub async fn toggle_pool_entry(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
    Path((namespace, step_id, version)): Path<(String, String, String)>,
    Json(body): Json<TogglePoolRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    // 收录即校验：enabling fetches the template content, verifies the pinned
    // sha256, and runs the save-time spec validation — a failed entry stays
    // staged and the curator sees why.
    if body.enabled {
        step_pool::verify_entry(
            &http_client(),
            &state.pool,
            &namespace,
            &step_id,
            &version,
            env!("CARGO_PKG_VERSION"),
        )
        .await
        .map_err(|e| AppError::ValidationFailed(e.to_string()))?;
    }
    let result = sqlx::query(
        "UPDATE step_pool SET enabled = ? \
         WHERE namespace = ? AND step_id = ? AND version = ?",
    )
    .bind(body.enabled as i64)
    .bind(&namespace)
    .bind(&step_id)
    .bind(&version)
    .execute(&state.pool)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("pool entry not found".into()));
    }
    Ok(Json(ApiResponse::success(json!({
        "namespace": namespace, "step_id": step_id,
        "version": version, "enabled": body.enabled,
    }))))
}
