//! Admin-editable platform settings (step-marketplace §6.8 Phase 0).
//!
//! Values live in the `platform_settings` table and override manager.toml
//! defaults; resolver-affecting keys are re-applied immediately so toggles
//! take effect without a restart.

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;

use crate::middleware::PlatformAdmin;
use crate::services::platform_settings::{
    self, KEY_STEP_INDEX_LOCAL_DIR, KEY_STEP_REVIEWED_MANIFEST, KEY_STEP_REVIEW_REQUIRED,
    KEY_STEP_SYNC_TIMEOUT,
};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct UpdateSettingRequest {
    pub value: String,
}

/// GET /api/v1/admin/settings — effective values, defaults, and override flags.
pub async fn get_settings(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let overrides = platform_settings::fetch_overrides(&state.pool).await;
    let item = |key: &'static str, kind: &'static str, default: String| {
        json!({
            "key": key,
            "kind": kind,
            "value": overrides.get(key).cloned().unwrap_or_else(|| default.clone()),
            "default": default,
            "overridden": overrides.contains_key(key),
        })
    };
    // Defaults are the EFFECTIVE fallbacks: what applies when no DB override
    // exists (e.g. an empty local_dir falls back to <data_dir>/plugins).
    // marketplace_url is deliberately NOT listed: it is a navigation link
    // (future plugin-detail deep links to the static site), consumed from
    // env/manager.toml via /health — not an operator knob here.
    let local_dir_default = crate::services::step_resolver::local_plugin_dir(
        &state.config.storage.data_dir,
        &state.config.step_index.local_dir,
    )
    .map(|p| p.to_string_lossy().into_owned())
    .unwrap_or_else(|e| format!("(invalid: {e})"));
    let mut items = vec![
        item(
            KEY_STEP_REVIEW_REQUIRED,
            "bool",
            state.config.step_index.require_reviewed.to_string(),
        ),
        item(KEY_STEP_INDEX_LOCAL_DIR, "path", local_dir_default),
        item(
            KEY_STEP_REVIEWED_MANIFEST,
            "path",
            // Empty config → the reviews.json beside the plugin directory
            // (one buz-plugins clone in the data volume provides both).
            if state.config.step_index.reviewed_manifest.trim().is_empty() {
                crate::services::step_resolver::reviewed_manifest_default(std::path::Path::new(
                    &state.config.storage.data_dir,
                ))
                .to_string_lossy()
                .into_owned()
            } else {
                state.config.step_index.reviewed_manifest.clone()
            },
        ),
        item(
            KEY_STEP_SYNC_TIMEOUT,
            "number",
            state.config.step_index.sync_timeout_seconds.to_string(),
        ),
    ];
    // The local step directory may be relative — show the absolute path it
    // resolves to (manager working directory) so operators are never guessing.
    if let Some(item) = items
        .iter_mut()
        .find(|i| i["key"] == KEY_STEP_INDEX_LOCAL_DIR)
    {
        let effective_dir = platform_settings::effective(
            &state.pool,
            KEY_STEP_INDEX_LOCAL_DIR,
            &state.config.step_index.local_dir,
        )
        .await;
        let resolved = crate::services::step_resolver::local_plugin_dir(
            &state.config.storage.data_dir,
            &effective_dir,
        )
        .map(|p| {
            // Relative paths depend on the manager's working directory —
            // surface the absolute form so operators see exactly where to look.
            if p.is_absolute() {
                p
            } else {
                std::env::current_dir().unwrap_or_default().join(p)
            }
        })
        .map(|p| {
            // Normalize away `./` segments for a clean display.
            let mut out = std::path::PathBuf::new();
            for component in p.components() {
                match component {
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        out.pop();
                    }
                    other => out.push(other.as_os_str()),
                }
            }
            out
        })
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|e| format!("(invalid: {e})"));
        item["resolved"] = serde_json::json!(resolved);
    }
    Ok(Json(ApiResponse::success(json!({ "items": items }))))
}

/// PUT /api/v1/admin/settings/{key} — validate, persist, apply.
pub async fn update_setting(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<UpdateSettingRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let value = body.value.trim().to_string();
    platform_settings::upsert(&state.pool, &key, &value, &auth.username)
        .await
        .map_err(|e| AppError::ValidationFailed(e.to_string()))?;
    reapply_step_index_overrides(&state).await?;
    Ok(Json(ApiResponse::success(
        json!({ "key": key, "value": value }),
    )))
}

/// DELETE /api/v1/admin/settings/{key} — clear the override, fall back to the
/// manager.toml default.
pub async fn reset_setting(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    platform_settings::delete(&state.pool, &key)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    reapply_step_index_overrides(&state).await?;
    Ok(Json(ApiResponse::success(json!({ "key": key }))))
}

/// Resolver-affecting keys must reach the step resolver immediately —
/// full-replace from the DB so keys without a row clear their override.
async fn reapply_step_index_overrides(state: &AppState) -> Result<(), AppError> {
    let overrides = platform_settings::fetch_overrides(&state.pool).await;
    let require_reviewed = overrides
        .get(KEY_STEP_REVIEW_REQUIRED)
        .and_then(|v| v.parse::<bool>().ok());
    let local_dir = overrides.get(KEY_STEP_INDEX_LOCAL_DIR).cloned();
    // The runtime slot always carries the EFFECTIVE manifest (DB override
    // or the data-root default), matching what startup init installed.
    let reviewed_manifest = crate::services::step_resolver::effective_reviewed_manifest(
        &state.config.storage.data_dir,
        &state.config.step_index.reviewed_manifest,
        overrides
            .get(KEY_STEP_REVIEWED_MANIFEST)
            .map(String::as_str),
    )
    .to_string_lossy()
    .into_owned();
    crate::services::step_resolver::apply_runtime_overrides(
        require_reviewed,
        local_dir,
        Some(reviewed_manifest),
    );
    Ok(())
}
