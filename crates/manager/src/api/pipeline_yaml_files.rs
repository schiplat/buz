//! Online YAML authoring against the database.
//!
//! The pipeline row is the authority: `yaml_source` holds the exact bytes the
//! validation accepted, and the task/binding projection is written in the same
//! path. Nothing is written to `pipeline.yaml_scan_dir`; that directory is only
//! an import/export location.
//!
//! Identifiers are flat origin basenames (`my-pipe.yml`).
//! Optimistic concurrency via content `etag` / `If-Match`.

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::rbac::Perm;

use crate::audit::{self, Action, AuditEvent};
use crate::middleware::AuthUser;
use crate::rbac::pipeline_team_id;
use crate::services::pipeline_scanner::{
    delete_yaml_file, filename_from_pipeline_name, filename_from_yaml_path, find_yaml_pipeline_row,
    list_yaml_files, read_yaml_file, validate_yaml_filename, write_and_upsert_yaml,
    yaml_content_etag,
};
use crate::services::yaml_validate::{
    parse_and_resolve_team, reject_if_invalid, validate_pipeline_yaml,
};
use crate::state::AppState;

#[derive(serde::Deserialize)]
pub struct WriteYamlFileRequest {
    pub yaml: String,
    /// Optional basename (`my-pipe.yml`). When omitted on create, derived from YAML `name:`.
    #[serde(default)]
    pub filename: Option<String>,
    /// When set, retarget this pipeline id to the file (Form/API → file migration).
    #[serde(default)]
    pub pipeline_id: Option<String>,
    /// Expected content ETag from last GET (optimistic lock). Also accepted via `If-Match`.
    #[serde(default)]
    pub etag: Option<String>,
}

pub async fn list_files(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let scan_dir = &state.config.pipeline.yaml_scan_dir;
    let files = list_yaml_files(&state.db()).await?;
    // Filter by team visibility (platform admin sees all).
    let visible: Vec<_> = files
        .into_iter()
        .filter(|f| match &f.team_id {
            None => auth.is_platform_admin, // unbound file: admin-only
            Some(tid) => auth.check_team(tid, Perm::TeamView).is_ok(),
        })
        .collect();
    Ok(Json(ApiResponse::success(serde_json::json!({
        "scan_dir": scan_dir,
        "files": visible,
    }))))
}

pub async fn get_file(
    auth: AuthUser,
    Path(filename): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let scan_dir = &state.config.pipeline.yaml_scan_dir;
    validate_yaml_filename(&filename)?;
    authorize_file_access(&state, &auth, scan_dir, &filename, Perm::TeamView).await?;
    let yaml = read_yaml_file(&state.db(), &filename).await?;
    let yaml_path = filename.clone();
    let etag = yaml_content_etag(&yaml);
    Ok(Json(ApiResponse::success(serde_json::json!({
        "filename": filename,
        "yaml_path": yaml_path,
        "yaml": yaml,
        "etag": etag,
    }))))
}

/// Read YAML content for a pipeline by id (`source=yaml` + `yaml_path`).
pub async fn get_pipeline_source(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = pipeline_team_id(&state.db(), &id).await?;
    auth.check_team(&team_id, Perm::TeamView)?;

    let (source, yaml_path, name) =
        crate::store::pipelines::pipeline_source_row(&state.db(), &id).await?;

    if source != "yaml" {
        return Err(AppError::ValidationFailed(
            "pipeline is not file-backed (source != yaml); save YAML to migrate to the scan directory"
                .into(),
        ));
    }
    let yaml_path =
        yaml_path.ok_or_else(|| AppError::NotFound("pipeline has no yaml_path".into()))?;
    let filename = filename_from_yaml_path(&yaml_path)
        .ok_or_else(|| AppError::Internal(format!("invalid yaml_path on pipeline: {yaml_path}")))?;

    let yaml = read_yaml_file(&state.db(), &filename).await?;
    let etag = yaml_content_etag(&yaml);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "pipeline_id": id,
        "name": name,
        "filename": filename,
        "yaml_path": filename,
        "yaml": yaml,
        "etag": etag,
    }))))
}

pub async fn create_file(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<WriteYamlFileRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let yaml = req.yaml;
    if yaml.trim().is_empty() {
        return Err(AppError::ValidationFailed("yaml is required".into()));
    }

    // Target team from serde (same as upsert) — authorize BEFORE any write.
    let (parsed, target_team_id, _) = parse_and_resolve_team(&state.db(), &yaml).await?;
    auth.check_team(&target_team_id, Perm::PipelineWrite)?;

    let validation = validate_pipeline_yaml(&state.db(), &yaml).await?;
    reject_if_invalid(&validation)?;
    let pipeline_name = parsed.name.trim();
    let pipeline_name = if pipeline_name.is_empty() {
        "pipeline"
    } else {
        pipeline_name
    };

    let filename = match req
        .filename
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(f) => {
            validate_yaml_filename(f)?;
            f.to_string()
        }
        None => filename_from_pipeline_name(pipeline_name),
    };

    // Create refuses overwrite — the origin basename identifies at most one row.
    if find_yaml_pipeline_row(&state.db(), &filename)
        .await?
        .is_some()
    {
        return Err(AppError::Conflict(format!(
            "file {filename} already exists — use PUT to update"
        )));
    }

    let result = write_and_upsert_yaml(
        &state.db(),
        &filename,
        &yaml,
        &crate::services::plugin_resolver::effective_local_dir(
            &state.config.storage.data_dir,
            &state.config.plugin_index.local_dir,
        )
        .map_err(|e| AppError::Internal(e.to_string()))?,
        None,
        crate::services::platform_settings::effective(
            &state.db(),
            crate::services::platform_settings::KEY_PLUGIN_SYNC_TIMEOUT,
            &state.config.plugin_index.sync_timeout_seconds.to_string(),
        )
        .await
        .parse()
        .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS),
        None,
        true,
    )
    .await?;
    let etag = yaml_content_etag(&yaml);

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::PipelineYamlCreate)
            .resource("pipeline", &result.pipeline_id, &result.name)
            .team(Some(target_team_id))
            .after(Some(serde_json::json!({
                "filename": result.filename,
                "yaml_path": result.yaml_path,
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "pipeline_id": result.pipeline_id,
        "filename": result.filename,
        "yaml_path": result.yaml_path,
        "name": result.name,
        "etag": etag,
    }))))
}

pub async fn put_file(
    auth: AuthUser,
    Path(filename): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<WriteYamlFileRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let scan_dir = state.config.pipeline.yaml_scan_dir.clone();
    validate_yaml_filename(&filename)?;
    let yaml = req.yaml;
    if yaml.trim().is_empty() {
        return Err(AppError::ValidationFailed("yaml is required".into()));
    }

    // Target team from serde — must authorize BEFORE write/upsert (no post-hoc orphan).
    let (_parsed, target_team_id, _) = parse_and_resolve_team(&state.db(), &yaml).await?;
    auth.check_team(&target_team_id, Perm::PipelineWrite)?;

    // Also require write on the current binding (source team / migrate source).
    if let Some(ref pid) = req.pipeline_id {
        let current_team_id = pipeline_team_id(&state.db(), pid).await?;
        auth.check_team(&current_team_id, Perm::PipelineWrite)?;
    } else {
        authorize_file_access(&state, &auth, &scan_dir, &filename, Perm::PipelineWrite).await?;
    }

    // Optimistic concurrency: this early check gives a clean 409 without doing
    // any resolution work, and the write path re-checks the same token under
    // its write transaction so two writers cannot both pass.
    let expected_etag = if_match_value(&headers, req.etag.as_deref());
    if find_yaml_pipeline_row(&state.db(), &filename)
        .await?
        .is_some()
    {
        if let Some(ref expected) = expected_etag {
            let current = read_yaml_file(&state.db(), &filename).await?;
            let current_etag = yaml_content_etag(&current);
            if expected != "*" && *expected != current_etag {
                return Err(AppError::Conflict(
                    "YAML definition was modified by another writer — reload and retry (If-Match / etag mismatch)"
                        .into(),
                ));
            }
        }
    }

    let validation = validate_pipeline_yaml(&state.db(), &yaml).await?;
    reject_if_invalid(&validation)?;

    let result = write_and_upsert_yaml(
        &state.db(),
        &filename,
        &yaml,
        &crate::services::plugin_resolver::effective_local_dir(
            &state.config.storage.data_dir,
            &state.config.plugin_index.local_dir,
        )
        .map_err(|e| AppError::Internal(e.to_string()))?,
        req.pipeline_id.as_deref(),
        crate::services::platform_settings::effective(
            &state.db(),
            crate::services::platform_settings::KEY_PLUGIN_SYNC_TIMEOUT,
            &state.config.plugin_index.sync_timeout_seconds.to_string(),
        )
        .await
        .parse()
        .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS),
        expected_etag.as_deref(),
        false,
    )
    .await?;
    let etag = yaml_content_etag(&yaml);

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::PipelineYamlUpdate)
            .resource("pipeline", &result.pipeline_id, &result.name)
            .team(Some(target_team_id))
            .after(Some(serde_json::json!({
                "filename": result.filename,
                "yaml_path": result.yaml_path,
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "pipeline_id": result.pipeline_id,
        "filename": result.filename,
        "yaml_path": result.yaml_path,
        "name": result.name,
        "etag": etag,
    }))))
}

pub async fn delete_file(
    auth: AuthUser,
    Path(filename): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let scan_dir = &state.config.pipeline.yaml_scan_dir;
    validate_yaml_filename(&filename)?;
    authorize_file_access(&state, &auth, scan_dir, &filename, Perm::PipelineDelete).await?;

    let deleted_id = delete_yaml_file(&state.db(), &filename).await?;

    if let Some(ref id) = deleted_id {
        audit::record(
            &state.pool,
            AuditEvent::from_auth(&auth, Action::PipelineYamlDelete)
                .resource("pipeline", id, &filename)
                .after(Some(serde_json::json!({ "filename": filename }))),
        )
        .await;
    }

    Ok(Json(ApiResponse::success(())))
}

fn if_match_value(headers: &HeaderMap, body_etag: Option<&str>) -> Option<String> {
    body_etag
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            headers
                .get(axum::http::header::IF_MATCH)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
}

async fn authorize_file_access(
    state: &AppState,
    auth: &AuthUser,
    _scan_dir: &str,
    filename: &str,
    perm: Perm,
) -> Result<(), AppError> {
    match find_yaml_pipeline_row(&state.db(), filename).await? {
        Some(pid) => {
            let team_id = pipeline_team_id(&state.db(), &pid).await?;
            auth.check_team(&team_id, perm)?;
        }
        None => {
            if auth.is_platform_admin {
                return Ok(());
            }
            if matches!(perm, Perm::TeamView) {
                return Err(AppError::Forbidden(
                    "unbound YAML file requires platform admin".into(),
                ));
            }
            let can_write = auth
                .memberships
                .iter()
                .any(|m| auth.check_team(&m.team_id, Perm::PipelineWrite).is_ok());
            if !can_write {
                return Err(AppError::Forbidden(
                    "PipelineWrite required to create YAML files".into(),
                ));
            }
        }
    }
    Ok(())
}
