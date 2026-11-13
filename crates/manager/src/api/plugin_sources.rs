//! Admin management for plugin sources and the template pool
//! (plugin-marketplace §6.8 Phase 1).

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent, Resource};
use crate::middleware::PlatformAdmin;
use crate::services::plugin_pool;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateSourceRequest {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub auto_enable: bool,
    /// Optional mirror origin — tried before `url` on every fetch. Same
    /// layout as the canonical index (its `index.json` URL). Content is
    /// sha256-pinned, so a mirror cannot tamper with verified templates.
    #[serde(default)]
    pub mirror_url: Option<String>,
}

#[derive(Deserialize)]
pub struct TogglePoolRequest {
    pub enabled: bool,
}

/// GET /api/v1/admin/plugin-sources — all configured sources with sync state.
pub async fn list_sources(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let items = plugin_pool::list_sources(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::success(json!({ "items": items }))))
}

/// POST /api/v1/admin/plugin-sources — add a source and sync it immediately.
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

    let mirror_url = body
        .mirror_url
        .as_deref()
        .map(str::trim)
        .filter(|mirror| !mirror.is_empty())
        .map(str::to_string);
    if let Some(mirror) = &mirror_url {
        if !mirror.starts_with("https://") && !mirror.starts_with("http://") {
            return Err(AppError::ValidationFailed(
                "mirror_url must be an absolute http(s) index.json URL".into(),
            ));
        }
    }

    let id = Uuid::new_v4().to_string();
    crate::store::plugin_sources::insert(
        &state.db(),
        &id,
        &name,
        &url,
        mirror_url.as_deref(),
        body.auto_enable,
        &auth.username,
    )
    .await?;
    audit::record(
        &state.pool,
        AuditEvent::new(Action::PluginSourceCreate, &auth)
            .on(Resource::plugin_source(&id, &name))
            .after(Some(json!({
                "name": name,
                "url": url,
                "mirror_url": mirror_url,
                "auto_enable": body.auto_enable,
            }))),
    )
    .await;

    // Sync runs in the background: the API returns the staged source
    // immediately and the UI polls the list for last_synced_at / error.
    let pool = state.pool.clone();
    let timeout_secs = crate::services::platform_settings::effective(
        &state.pool,
        crate::services::platform_settings::KEY_PLUGIN_SYNC_TIMEOUT,
        &state.config.plugin_index.sync_timeout_seconds.to_string(),
    )
    .await
    .parse()
    .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS);
    let source = plugin_pool::get_source(&state.pool, &id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or(AppError::NotFound("source disappeared after insert".into()))?;

    let spawn_source = source.clone();
    tokio::spawn(async move {
        let http = plugin_pool::http_client(timeout_secs);
        if let Err(e) =
            plugin_pool::sync_source(&http, &pool, &spawn_source, shared::version::VERSION).await
        {
            tracing::warn!(source_id = %spawn_source.id, error = %e, "background source sync failed");
        }
    });

    Ok(Json(ApiResponse::success(json!({
        "source": source,
        "syncing": true,
    }))))
}

/// POST /api/v1/admin/plugin-sources/{id}/mirror-check — compare the namespace
/// revision sentinel between the mirror and the canonical origin (tiny GETs;
/// falls back to a full index compare when a sentinel is missing).
pub async fn check_mirror(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let timeout_secs = crate::services::platform_settings::effective(
        &state.pool,
        crate::services::platform_settings::KEY_PLUGIN_SYNC_TIMEOUT,
        &state.config.plugin_index.sync_timeout_seconds.to_string(),
    )
    .await
    .parse()
    .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS);
    let http = plugin_pool::http_client(timeout_secs);
    let report = plugin_pool::check_mirror(&http, &state.pool, &id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::success(
        serde_json::to_value(&report).map_err(|e| AppError::Internal(e.to_string()))?,
    )))
}

/// POST /api/v1/admin/plugin-sources/{id}/sync — re-fetch the index.
pub async fn sync_source(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let source = plugin_pool::get_source(&state.pool, &id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or(AppError::NotFound("source not found".into()))?;
    let pool = state.pool.clone();
    let timeout_secs = crate::services::platform_settings::effective(
        &state.pool,
        crate::services::platform_settings::KEY_PLUGIN_SYNC_TIMEOUT,
        &state.config.plugin_index.sync_timeout_seconds.to_string(),
    )
    .await
    .parse()
    .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS);
    audit::record(
        &state.pool,
        AuditEvent::new(Action::PluginSourceSync, &auth)
            .on(Resource::plugin_source(&source.id, &source.name))
            .meta(Some(json!({ "trigger": "manual" }))),
    )
    .await;
    tokio::spawn(async move {
        let http = plugin_pool::http_client(timeout_secs);
        if let Err(e) =
            plugin_pool::sync_source(&http, &pool, &source, shared::version::VERSION).await
        {
            tracing::warn!(source_id = %source.id, error = %e, "background source sync failed");
        }
    });
    Ok(Json(ApiResponse::success(
        json!({ "id": id, "syncing": true }),
    )))
}

/// POST /api/v1/admin/plugin-sources/{id}/reset-index-watermark — forget the
/// highest signed catalog sequence accepted for this source, so the next sync
/// re-establishes it. Only needed when a publisher legitimately restarted its
/// numbering (the anti-rollback check would otherwise hold the source back);
/// it re-opens the replay window until that next verified sync.
pub async fn reset_index_watermark(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let source = plugin_pool::get_source(&state.pool, &id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("source not found".into()))?;
    crate::store::plugin_sources::reset_index_watermark(&state.db(), &id).await?;
    audit::record(
        &state.pool,
        AuditEvent::new(Action::PluginSourceResetIndexWatermark, &auth)
            .on(Resource::plugin_source(&source.id, &source.name))
            .changes(
                Some(json!({
                    "last_index_sequence": source.last_index_sequence,
                    "last_index_issued_at": source.last_index_issued_at,
                })),
                Some(json!({
                    "last_index_sequence": null,
                    "last_index_issued_at": null,
                })),
            ),
    )
    .await;
    Ok(Json(ApiResponse::success(json!({ "id": id }))))
}

/// GET /api/v1/admin/plugin-sources/{id}/sync-progress — live sync progress for
/// the dashboard's progress bar. `phase: "idle"` = no sync seen this run.
pub async fn sync_progress(
    _auth: PlatformAdmin,
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let progress = crate::services::sync_progress::snapshot(&id);
    Ok(Json(ApiResponse::success(match progress {
        Some(progress) => {
            serde_json::to_value(progress).map_err(|e| AppError::Internal(e.to_string()))?
        }
        None => json!({ "phase": "idle" }),
    })))
}

/// DELETE /api/v1/admin/plugin-sources/{id} — removes the source and its pool
/// rows. Already-saved pipelines are unaffected (resolution froze at save).
pub async fn delete_source(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    // Snapshot before deleting: once the row is gone (CASCADE also drops its
    // pool rows) the audit record is the only remaining description of it.
    let removed = plugin_pool::get_source(&state.pool, &id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    crate::store::plugin_sources::delete(&state.db(), &id).await?;
    if let Some(removed) = &removed {
        audit::record(
            &state.pool,
            AuditEvent::new(Action::PluginSourceDelete, &auth)
                .on(Resource::plugin_source(&removed.id, &removed.name))
                .before(Some(json!({
                    "name": removed.name,
                    "url": removed.url,
                    "mirror_url": removed.mirror_url,
                    "auto_enable": removed.auto_enable,
                }))),
        )
        .await;
    }
    // Deleting the factory-seeded source is a deliberate operator choice
    // (typically the official site is unreachable); remember it so the
    // startup migration does not re-seed it on every boot.
    if id == "official" {
        crate::store::plugin_sources::mark_official_source_dismissed(&state.db(), &auth.username)
            .await?;
    }
    Ok(Json(ApiResponse::success(json!({ "id": id }))))
}

/// POST /api/v1/admin/plugin-pool/{namespace}/{plugin_id}/refresh — re-fetch the
/// source index and upsert only this plugin's entries (curator's enable
/// state survives). Returns the refreshed version rows.
pub async fn refresh_plugin(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Path((namespace, plugin_id)): Path<(String, String)>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let http = plugin_pool::http_client(
        crate::services::platform_settings::effective(
            &state.pool,
            crate::services::platform_settings::KEY_PLUGIN_SYNC_TIMEOUT,
            &state.config.plugin_index.sync_timeout_seconds.to_string(),
        )
        .await
        .parse()
        .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS),
    );
    let rows = plugin_pool::refresh_plugin(
        &http,
        &state.pool,
        &namespace,
        &plugin_id,
        shared::version::VERSION,
    )
    .await
    .map_err(|e| AppError::ValidationFailed(e.to_string()))?;
    audit::record(
        &state.pool,
        AuditEvent::new(Action::PluginPoolRefresh, &auth)
            .on(Resource::plugin_pool_entry(&namespace, &plugin_id))
            .meta(Some(json!({ "versions": rows.len() }))),
    )
    .await;
    Ok(Json(ApiResponse::success(json!({ "items": rows }))))
}

/// GET /api/v1/admin/plugin-pool — every pool row (enabled + staged).
pub async fn list_pool(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let items = plugin_pool::list_pool(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::success(json!({ "items": items }))))
}

/// PUT /api/v1/admin/plugin-pool/{namespace}/{plugin_id}/{version} — enable or
/// disable one pool entry (the curator's curation action).
pub async fn toggle_pool_entry(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Path((namespace, plugin_id, version)): Path<(String, String, String)>,
    Json(body): Json<TogglePoolRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    // 收录即校验：enabling fetches the template content, verifies the pinned
    // sha256, and runs the save-time spec validation — a failed entry stays
    // staged and the curator sees why.
    if body.enabled {
        plugin_pool::verify_entry(
            &plugin_pool::http_client(
                crate::services::platform_settings::effective(
                    &state.pool,
                    crate::services::platform_settings::KEY_PLUGIN_SYNC_TIMEOUT,
                    &state.config.plugin_index.sync_timeout_seconds.to_string(),
                )
                .await
                .parse()
                .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS),
            ),
            &state.pool,
            &namespace,
            &plugin_id,
            &version,
            env!("CARGO_PKG_VERSION"),
        )
        .await
        .map_err(|e| AppError::ValidationFailed(e.to_string()))?;
    }
    // State change, provenance and audit land together (store owns the tx).
    crate::store::plugin_sources::toggle_pool_entry(
        &state.db(),
        &namespace,
        &plugin_id,
        &version,
        body.enabled,
        &auth,
    )
    .await?;
    Ok(Json(ApiResponse::success(json!({
        "namespace": namespace, "plugin_id": plugin_id,
        "version": version, "enabled": body.enabled,
    }))))
}
