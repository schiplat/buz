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

    // Sync runs in the background: the API returns the staged source
    // immediately and the UI polls the list for last_synced_at / error.
    let pool = state.pool.clone();
    let timeout_secs = crate::services::platform_settings::effective(
        &state.pool,
        crate::services::platform_settings::KEY_STEP_SYNC_TIMEOUT,
        &state.config.step_index.sync_timeout_seconds.to_string(),
    )
    .await
    .parse()
    .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS);
    let source = step_pool::get_source(&state.pool, &id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or(AppError::NotFound("source disappeared after insert".into()))?;

    let spawn_source = source.clone();
    tokio::spawn(async move {
        let http = step_pool::http_client(timeout_secs);
        if let Err(e) = step_pool::sync_source(&http, &pool, &spawn_source).await {
            tracing::warn!(source_id = %spawn_source.id, error = %e, "background source sync failed");
        }
    });

    Ok(Json(ApiResponse::success(json!({
        "source": source,
        "syncing": true,
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
    let pool = state.pool.clone();
    let timeout_secs = crate::services::platform_settings::effective(
        &state.pool,
        crate::services::platform_settings::KEY_STEP_SYNC_TIMEOUT,
        &state.config.step_index.sync_timeout_seconds.to_string(),
    )
    .await
    .parse()
    .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS);
    tokio::spawn(async move {
        let http = step_pool::http_client(timeout_secs);
        if let Err(e) = step_pool::sync_source(&http, &pool, &source).await {
            tracing::warn!(source_id = %source.id, error = %e, "background source sync failed");
        }
    });
    Ok(Json(ApiResponse::success(
        json!({ "id": id, "syncing": true }),
    )))
}

/// DELETE /api/v1/admin/step-sources/{id} — removes the source and its pool
/// rows. Already-saved pipelines are unaffected (resolution froze at save).
pub async fn delete_source(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    sqlx::query("DELETE FROM step_sources WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    // Deleting the factory-seeded source is a deliberate operator choice
    // (typically the official site is unreachable); remember it so the
    // startup migration does not re-seed it on every boot.
    if id == "official" {
        sqlx::query(
            "INSERT INTO platform_settings (key, value, updated_by, updated_at) \
             VALUES ('step_official_source_dismissed', 'true', ?, datetime('now')) \
             ON CONFLICT(key) DO UPDATE SET value = 'true', updated_by = excluded.updated_by, \
               updated_at = excluded.updated_at",
        )
        .bind(&auth.username)
        .execute(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    }
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
            &step_pool::http_client(
                crate::services::platform_settings::effective(
                    &state.pool,
                    crate::services::platform_settings::KEY_STEP_SYNC_TIMEOUT,
                    &state.config.step_index.sync_timeout_seconds.to_string(),
                )
                .await
                .parse()
                .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS),
            ),
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
