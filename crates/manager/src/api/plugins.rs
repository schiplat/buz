//! Step template diagnostics for the local index (marketplace ADR §6.7).
//! Read-only: templates are authored in the buz-plugins repo and arrive via
//! git clone; ingestion gatekeeping lives there, not here.

use axum::{extract::State, Json};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use shared::error::AppError;
use shared::plugin_spec::{validate_plugin_spec, PluginSpec};
use shared::protocol::response::ApiResponse;

use crate::middleware::AuthUser;
use crate::state::AppState;

/// GET /api/v1/plugins — list every parseable template in the local plugin
/// directory, with attribution, inputs, and search metadata. Unparseable
/// files are reported separately so operators can fix or remove them
/// (`buz-manager plugin-validate` prints the details).
pub async fn list_plugins(
    _auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let dir = crate::services::plugin_resolver::effective_local_dir(
        &state.config.storage.data_dir,
        &state.config.plugin_index.local_dir,
    )
    .map_err(|e| AppError::Internal(e.to_string()))?;

    let files = match shared::yaml_files::discover_yaml_files(&dir) {
        Ok(files) => files,
        Err(_) => {
            // No step directory yet — an empty library, not an error.
            return Ok(Json(ApiResponse::success(json!({
                "items": [],
                "unparseable": [],
            }))));
        }
    };

    let reviewed_hashes = crate::services::plugin_resolver::load_reviewed_hashes(
        &state.config.plugin_index.reviewed_manifest,
    );
    let mut items: Vec<Value> = Vec::new();
    let mut unparseable: Vec<Value> = Vec::new();

    for file in files {
        let file_name = file
            .strip_prefix(&dir)
            .unwrap_or(&file)
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/");
        let Ok(raw) = std::fs::read_to_string(&file) else {
            continue;
        };
        let content_sha256 = format!("{:x}", Sha256::digest(raw.as_bytes()));

        let spec: PluginSpec = match serde_yaml::from_str(&raw) {
            Ok(spec) => spec,
            Err(e) => {
                unparseable.push(json!({ "file": file_name, "error": e.to_string() }));
                continue;
            }
        };

        let buz_compatible = match spec.requires_buz.as_deref() {
            Some(raw) => {
                shared::plugin_spec::buz_requirement_satisfied(raw, env!("CARGO_PKG_VERSION"))
                    .is_ok()
            }
            None => true,
        };

        let validation = validate_plugin_spec(&spec, env!("CARGO_PKG_VERSION"));
        let reviewed = reviewed_hashes.contains(&content_sha256);
        let inputs: Vec<Value> = spec
            .inputs
            .iter()
            .flatten()
            .map(|(name, input)| {
                json!({
                    "name": name,
                    "type": input.input_type.clone().unwrap_or_else(|| "string".into()),
                    "required": input.required.unwrap_or(false),
                    "default": input.default,
                    "masking": input.masking.unwrap_or(false),
                    "description": input.description,
                    "pattern": input.pattern,
                    "options": input.options,
                })
            })
            .collect();

        items.push(json!({
            "id": spec.id,
            "version": spec.version,
            "file": file_name,
            "name": spec.name,
            "description": spec.description,
            "description_zh": spec.description_zh,
            "category": spec.category,
            "author": spec.author,
            "publisher": spec.effective_publisher(),
            "license": spec.license,
            "source_repo": spec.source_repo,
            "engine": spec.effective_engine(),
            "keywords": spec.keywords,
            "default_timeout": spec.default_timeout,
            "requires_buz": spec.requires_buz,
            "buz_compatible": buz_compatible,
            "content_sha256": content_sha256,
            "reviewed": reviewed,
            "valid": validation.is_ok(),
            "validation_errors": validation.err().unwrap_or_default(),
            "inputs": inputs,
        }));
    }

    items.sort_by_key(|item| {
        (
            item["id"].as_str().unwrap_or("").to_string(),
            item["version"].as_str().unwrap_or("").to_string(),
        )
    });

    Ok(Json(ApiResponse::success(json!({
        "items": items,
        "unparseable": unparseable,
    }))))
}
