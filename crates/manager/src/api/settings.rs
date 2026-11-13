//! Admin-editable platform settings (plugin-marketplace §6.8 Phase 0).
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

use crate::audit::{self, Action, AuditEvent, Resource};
use crate::middleware::PlatformAdmin;
use crate::services::platform_settings::{
    self, DEFAULT_JOB_LOG_RETENTION_DAYS, DEFAULT_SANDBOX_CONFIG_POLL_SECONDS,
    DEFAULT_SANDBOX_CONFIG_RETENTION_DAYS, DEFAULT_SANDBOX_CONFIG_STALE_SECONDS,
    KEY_GRANTS_REQUIRE_APPROVAL, KEY_JOB_LOG_RETENTION_DAYS, KEY_PLUGIN_INDEX_LOCAL_DIR,
    KEY_PLUGIN_REVIEWED_MANIFEST, KEY_PLUGIN_REVIEW_REQUIRED, KEY_PLUGIN_SYNC_TIMEOUT,
    KEY_SANDBOX_CONFIG_POLL_SECONDS, KEY_SANDBOX_CONFIG_REQUIRE_APPROVAL,
    KEY_SANDBOX_CONFIG_RETENTION_DAYS, KEY_SANDBOX_CONFIG_STALE_SECONDS,
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
    let overrides = platform_settings::fetch_overrides(&state.db()).await;
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
    let local_dir_default = crate::services::plugin_resolver::local_plugin_dir(
        &state.config.storage.data_dir,
        &state.config.plugin_index.local_dir,
    )
    .map(|p| p.to_string_lossy().into_owned())
    .unwrap_or_else(|e| format!("(invalid: {e})"));
    let mut items = vec![
        item(
            KEY_PLUGIN_REVIEW_REQUIRED,
            "bool",
            state.config.plugin_index.require_reviewed.to_string(),
        ),
        item(KEY_PLUGIN_INDEX_LOCAL_DIR, "path", local_dir_default),
        item(
            KEY_PLUGIN_REVIEWED_MANIFEST,
            "path",
            // Empty config → the reviews.json beside the plugin directory
            // (one buz-plugins clone in the data volume provides both).
            if state
                .config
                .plugin_index
                .reviewed_manifest
                .trim()
                .is_empty()
            {
                crate::services::plugin_resolver::reviewed_manifest_default(std::path::Path::new(
                    &state.config.storage.data_dir,
                ))
                .to_string_lossy()
                .into_owned()
            } else {
                state.config.plugin_index.reviewed_manifest.clone()
            },
        ),
        item(
            KEY_PLUGIN_SYNC_TIMEOUT,
            "number",
            state.config.plugin_index.sync_timeout_seconds.to_string(),
        ),
        item(KEY_SANDBOX_CONFIG_REQUIRE_APPROVAL, "bool", "false".into()),
        item(KEY_GRANTS_REQUIRE_APPROVAL, "bool", "false".into()),
        item(
            KEY_SANDBOX_CONFIG_RETENTION_DAYS,
            "number",
            DEFAULT_SANDBOX_CONFIG_RETENTION_DAYS.to_string(),
        ),
        item(
            KEY_SANDBOX_CONFIG_POLL_SECONDS,
            "number",
            DEFAULT_SANDBOX_CONFIG_POLL_SECONDS.to_string(),
        ),
        item(
            KEY_SANDBOX_CONFIG_STALE_SECONDS,
            "number",
            DEFAULT_SANDBOX_CONFIG_STALE_SECONDS.to_string(),
        ),
        item(
            KEY_JOB_LOG_RETENTION_DAYS,
            "number",
            DEFAULT_JOB_LOG_RETENTION_DAYS.to_string(),
        ),
    ];
    // Path settings carry existence/readability of the effective value so
    // the UI can warn (configured-but-missing, or unreadable due to perms).
    for item in items.iter_mut() {
        if item["kind"] == "path" {
            let raw = item["value"].as_str().unwrap_or_default().to_string();
            let default = item["default"].as_str().unwrap_or_default().to_string();
            let candidate = if raw.trim().is_empty() { default } else { raw };
            let trimmed = candidate.trim();
            if trimmed.is_empty() {
                item["file_state"] = serde_json::json!("unset");
            } else {
                let path = std::path::Path::new(trimmed);
                if !path.exists() {
                    item["file_state"] = serde_json::json!("missing");
                } else if path.is_file() && std::fs::read(path).is_err() {
                    item["file_state"] = serde_json::json!("unreadable");
                } else {
                    item["file_state"] = serde_json::json!("ok");
                }
            }
        }
    }

    // The local plugin directory may be relative — show the absolute path it
    // resolves to (manager working directory) so operators are never guessing.
    if let Some(item) = items
        .iter_mut()
        .find(|i| i["key"] == KEY_PLUGIN_INDEX_LOCAL_DIR)
    {
        let effective_dir = platform_settings::effective(
            &state.db(),
            KEY_PLUGIN_INDEX_LOCAL_DIR,
            &state.config.plugin_index.local_dir,
        )
        .await;
        let resolved = crate::services::plugin_resolver::local_plugin_dir(
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

    // Saving a manifest path that does not exist is always a mistake (the
    // ledger must be present) — fail fast with the same friendly framing.
    if key == platform_settings::KEY_PLUGIN_REVIEWED_MANIFEST
        && !value.is_empty()
        && !std::path::Path::new(&value).exists()
    {
        return Err(AppError::ValidationFailed(format!(
            "no file at {value} — clone buz-plugins or pick an existing reviews.json path"
        )));
    }

    // Guard the dangerous combo at the source: enabling the review gate
    // while the manifest file does not exist would reject EVERY pipeline
    // save (empty ledger). Refuse the toggle with guidance instead of
    // letting the operator brick saves and discover it via the banner.
    if key == crate::services::platform_settings::KEY_PLUGIN_REVIEW_REQUIRED && value == "true" {
        let overrides = platform_settings::fetch_overrides(&state.db()).await;
        let manifest_path = crate::services::plugin_resolver::effective_reviewed_manifest(
            &state.config.storage.data_dir,
            &state.config.plugin_index.reviewed_manifest,
            overrides
                .get(platform_settings::KEY_PLUGIN_REVIEWED_MANIFEST)
                .map(String::as_str),
        );
        if !manifest_path.exists() {
            return Err(AppError::ValidationFailed(
                "review gate needs the manifest file — set it up under 'Reviewed manifest' first"
                    .into(),
            ));
        }
    }

    let previous = platform_settings::fetch_overrides(&state.db())
        .await
        .get(&key)
        .cloned();
    platform_settings::upsert(&state.db(), &key, &value, &auth.username)
        .await
        .map_err(|e| AppError::ValidationFailed(e.to_string()))?;
    reapply_plugin_index_overrides(&state).await?;
    // `from` / `to` instead of named-setting keys: redact_value masks any key
    // literally named `value`, which would erase the very change we record.
    audit::record(
        &state.pool,
        AuditEvent::new(Action::SettingUpdate, &auth)
            .on(Resource::setting(&key))
            .changes(
                Some(json!({ "from": setting_display(&key, previous.as_deref()) })),
                Some(json!({ "to": setting_display(&key, Some(&value)) })),
            ),
    )
    .await;
    Ok(Json(ApiResponse::success(
        json!({ "key": key, "value": value }),
    )))
}

/// DELETE /api/v1/admin/settings/{key} — clear the override, fall back to the
/// manager.toml default.
pub async fn reset_setting(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let previous = platform_settings::fetch_overrides(&state.db())
        .await
        .get(&key)
        .cloned();
    platform_settings::delete(&state.db(), &key)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    reapply_plugin_index_overrides(&state).await?;
    audit::record(
        &state.pool,
        AuditEvent::new(Action::SettingReset, &auth)
            .on(Resource::setting(&key))
            .changes(
                Some(json!({ "from": setting_display(&key, previous.as_deref()) })),
                Some(json!({ "to": Value::Null })),
            ),
    )
    .await;
    Ok(Json(ApiResponse::success(json!({ "key": key }))))
}

/// Settings are audited by value, except where the key itself says the value
/// is a credential (none today — this is the guard for future settings).
fn setting_display(key: &str, value: Option<&str>) -> Value {
    let lower = key.to_ascii_lowercase();
    if ["secret", "token", "password", "key"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return json!("***");
    }
    match value {
        Some(value) => json!(value),
        None => Value::Null,
    }
}

/// Resolver-affecting keys must reach the plugin resolver immediately —
/// full-replace from the DB so keys without a row clear their override.
async fn reapply_plugin_index_overrides(state: &AppState) -> Result<(), AppError> {
    let overrides = platform_settings::fetch_overrides(&state.db()).await;
    let require_reviewed = overrides
        .get(KEY_PLUGIN_REVIEW_REQUIRED)
        .and_then(|v| v.parse::<bool>().ok());
    let local_dir = overrides.get(KEY_PLUGIN_INDEX_LOCAL_DIR).cloned();
    // The runtime slot always carries the EFFECTIVE manifest (DB override
    // or the data-root default), matching what startup init installed.
    let reviewed_manifest = crate::services::plugin_resolver::effective_reviewed_manifest(
        &state.config.storage.data_dir,
        &state.config.plugin_index.reviewed_manifest,
        overrides
            .get(KEY_PLUGIN_REVIEWED_MANIFEST)
            .map(String::as_str),
    )
    .to_string_lossy()
    .into_owned();
    crate::services::plugin_resolver::apply_runtime_overrides(
        require_reviewed,
        local_dir,
        Some(reviewed_manifest),
    );
    Ok(())
}
