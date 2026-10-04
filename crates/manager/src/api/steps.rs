//! Step template library listing (marketplace ADR §6.7) — the Dashboard
//! step-library page reads the local step directory through this endpoint.
//! Read-only: templates are authored in the buz-plugins repo and arrive via
//! git clone; ingestion gatekeeping lives there, not here.

use axum::{extract::State, Json};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::step_spec::StepSpec;

use crate::middleware::AuthUser;
use crate::state::AppState;

/// GET /api/v1/steps — list every parseable template in the local step
/// directory, with attribution, inputs, and search metadata. Unparseable
/// files are reported separately so operators can fix or remove them
/// (`buz-manager step-validate` prints the details).
pub async fn list_steps(
    _auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let dir = crate::services::step_resolver::local_step_index_dir(
        &state.config.pipeline.yaml_scan_dir,
        &state.config.step_index.local_dir,
    );

    let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("yaml") | Some("yml")
                )
            })
            .collect(),
        Err(_) => {
            // No step directory yet — an empty library, not an error.
            return Ok(Json(ApiResponse::success(json!({
                "items": [],
                "unparseable": [],
            }))));
        }
    };
    files.sort();

    let mut items: Vec<Value> = Vec::new();
    let mut unparseable: Vec<Value> = Vec::new();

    for file in files {
        let Ok(raw) = std::fs::read_to_string(&file) else {
            continue;
        };
        let file_name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let content_sha256 = format!("{:x}", Sha256::digest(raw.as_bytes()));

        let spec: StepSpec = match serde_yaml::from_str(&raw) {
            Ok(spec) => spec,
            Err(e) => {
                unparseable.push(json!({ "file": file_name, "error": e.to_string() }));
                continue;
            }
        };

        let buz_compatible = match spec.requires_buz.as_deref() {
            Some(raw) => {
                shared::step_spec::buz_requirement_satisfied(raw, env!("CARGO_PKG_VERSION")).is_ok()
            }
            None => true,
        };

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
