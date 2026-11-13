use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{
    masking_input_names, redact_input_map, validate_input_values, CreatePipelineRequest,
    PipelineInputDef, StepDef,
};
use shared::rbac::Perm;
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent};
use crate::crypto;
use crate::middleware::AuthUser;
use crate::rbac::{pipeline_team_id, resolve_required_team_id, secret_team_id};
use crate::state::AppState;

/// Success jobs ÷ total jobs for a pipeline. `None` when there are no builds yet.
pub fn job_success_rate(success_count: i64, total_count: i64) -> Option<f64> {
    if total_count <= 0 {
        None
    } else {
        Some(success_count as f64 / total_count as f64)
    }
}

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    // Debug (unredacted) jobs are admin-only end to end: non-admins must not
    // even see them as "latest job" or in the run totals of a pipeline card.
    let visible = if auth.is_platform_admin {
        None
    } else {
        Some(auth.team_ids())
    };
    let rows =
        crate::store::pipelines::list_rows(&state.db(), visible.as_deref(), auth.is_platform_admin)
            .await?;

    let pipelines: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let input_defs = serde_json::from_str::<serde_json::Value>(&r.input_defs).ok();
            serde_json::json!({
                "id": r.id,
                "name": r.name,
                "description": r.description,
                "trigger_type": r.trigger_type,
                "runner_labels": serde_json::from_str::<Vec<String>>(&r.runner_labels).unwrap_or_default(),
                "timeout_seconds": r.timeout_seconds,
                "concurrency_group": r.concurrency_group,
                "input_defs": input_defs,
                "is_active": r.is_active != 0,
                "source": r.source,
                "team_id": r.team_id,
                "owner_group": r.owner_group,
                "created_at": r.created_at,
                "updated_at": r.updated_at,
                "latest_job_id": r.latest_job_id,
                "latest_job_status": r.latest_job_status,
                "latest_job_start": r.latest_job_start,
                "job_total": r.job_total,
                "job_success": r.job_success,
                "success_rate": job_success_rate(r.job_success, r.job_total),
                "concurrency_mode": r.concurrency_mode,
                "concurrency_max_queue": r.concurrency_max_queue,
                "concurrency_queue_timeout_seconds": r.concurrency_queue_timeout_seconds,
                "yaml_path": r.yaml_path,
                "environments": serde_json::from_str::<Vec<String>>(&r.environments).unwrap_or_default(),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(pipelines)))
}

pub async fn create(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreatePipelineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let (team_id, owner_group) = resolve_required_team_id(
        &state.pool,
        req.team_id.as_deref(),
        req.owner_group.as_deref(),
    )
    .await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;
    verify_pipeline_secret_bindings(&state, &auth, &team_id, req.secret_ids.as_ref()).await?;
    super::environments::validate_environment_slugs(&state.db(), &req.environments).await?;
    let webhook_secret = validate_webhook_secret_binding(
        &state,
        &team_id,
        &req.trigger_type,
        req.webhook_secret.as_deref(),
    )
    .await?;

    // Form retirement: the request is rendered into the pipeline's YAML file
    // (the storage authority); the DB row starts as `form` and the bind step
    // below retargets it to `yaml` in the same save. On any failure the row
    // is removed, so a failed save never leaves a half-created pipeline.
    let pipeline_id = Uuid::new_v4().to_string();

    let mut secret_names: HashMap<String, String> = HashMap::new();
    for secret_id in req.secret_ids.iter().flatten() {
        if let Some(name) =
            crate::store::pipelines::secret_name_in_team(&state.db(), secret_id, &team_id).await?
        {
            secret_names.insert(secret_id.clone(), name);
        }
    }
    let mut var_names: HashMap<String, String> = HashMap::new();
    for var_id in req.var_ids.iter().flatten() {
        if let Some(name) =
            crate::store::variables::variable_name_optional(&state.db(), var_id).await?
        {
            var_names.insert(var_id.clone(), name);
        }
    }

    let yaml = crate::services::pipeline_yaml_emit::emit_from_request(
        &req,
        &owner_group,
        &secret_names,
        &var_names,
    )?;

    let environments_json =
        serde_json::to_string(&req.environments).unwrap_or_else(|_| "[]".into());
    let labels_json = serde_json::to_string(&req.runner_labels).unwrap_or_else(|_| "[]".into());
    let input_defs_json = serde_json::to_string(&req.input_defs.clone().unwrap_or_default())
        .unwrap_or_else(|_| "[]".into());
    let notify_json = serde_json::to_string(&req.notify).unwrap_or_else(|_| "[]".into());

    let db = state.db();
    let mut tx = db.begin_immediate().await?;
    crate::store::pipelines::insert_tx(
        &mut tx,
        &crate::store::pipelines::NewPipeline {
            id: pipeline_id.clone(),
            name: req.name.clone(),
            description: req.description.clone().unwrap_or_default(),
            trigger_type: req.trigger_type.clone(),
            webhook_path: req.webhook_path.clone(),
            webhook_secret: Some(webhook_secret.clone()),
            cron_expr: req.cron_expr.clone(),
            runner_labels: labels_json,
            timeout_seconds: req.timeout_seconds.unwrap_or(3600) as i64,
            concurrency_group: req.concurrency_group.clone(),
            concurrency_mode: req.concurrency_mode.clone().unwrap_or_default(),
            concurrency_max_queue: req.concurrency_max_queue.unwrap_or(3) as i64,
            concurrency_queue_timeout_seconds: req.concurrency_queue_timeout_seconds.unwrap_or(1800)
                as i64,
            owner_group: owner_group.clone(),
            team_id: team_id.clone(),
            input_defs: input_defs_json,
            environments: environments_json,
            notify_json,
        },
    )
    .await?;
    tx.commit().await?;

    // Bind the row to its definition: the upsert rewrites every field from the
    // YAML (source='yaml'), resolves `uses:` and records provenance.
    // Names are not unique; origin filenames are. Resolve a free one before
    // writing (`foo.yml`, `foo-2.yml`, …).
    let base = crate::services::pipeline_scanner::filename_from_pipeline_name(&req.name);
    let mut filename = base.clone();
    let mut suffix = 1u32;
    while crate::services::pipeline_scanner::find_yaml_pipeline_row(&state.pool, &filename)
        .await?
        .is_some()
    {
        suffix += 1;
        let stem = base.strip_suffix(".yml").unwrap_or(&base);
        filename = format!("{stem}-{suffix}.yml");
    }
    let write_result = crate::services::pipeline_scanner::write_and_upsert_yaml(
        &state.pool,
        &filename,
        &yaml,
        &crate::services::plugin_resolver::effective_local_dir(
            &state.config.storage.data_dir,
            &state.config.plugin_index.local_dir,
        )
        .map_err(|e| AppError::Internal(e.to_string()))?,
        Some(&pipeline_id),
        crate::services::platform_settings::effective(
            &state.db(),
            crate::services::platform_settings::KEY_PLUGIN_SYNC_TIMEOUT,
            &state.config.plugin_index.sync_timeout_seconds.to_string(),
        )
        .await
        .parse()
        .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS),
        None,
        false,
    )
    .await;
    if let Err(error) = write_result {
        // Compensate: the row shell is removed with the pipeline.
        let _ = crate::store::pipelines::delete(&state.db(), &pipeline_id).await;
        tracing::warn!(
            pipeline = %pipeline_id,
            error = %error,
            "pipeline create: YAML bind failed — row removed"
        );
        return Err(error);
    }

    tracing::info!(
        "Pipeline created: {} ({pipeline_id}) → {}",
        req.name,
        filename
    );

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::PipelineCreate)
            .resource("pipeline", &pipeline_id, &req.name)
            .team(Some(team_id.clone()))
            .after(Some(serde_json::json!({
                "id": pipeline_id,
                "name": req.name,
                "trigger_type": req.trigger_type,
                "team_id": team_id,
                "owner_group": owner_group,
                "step_count": req.steps.len(),
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": pipeline_id,
        "name": req.name,
        "description": req.description,
        "trigger_type": req.trigger_type,
        "runner_labels": req.runner_labels,
        "timeout_seconds": req.timeout_seconds.unwrap_or(3600),
        "concurrency_group": req.concurrency_group,
        "owner_group": owner_group,
        "team_id": team_id,
        "environments": req.environments,
        "file": filename,
        "steps": req.steps.iter().map(|step| serde_json::json!({
            "name": step.name,
            "engine": step.engine,
            "uses": step.uses,
        })).collect::<Vec<_>>(),
    }))))
}

pub async fn get_one(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::TeamView)?;

    let r = crate::store::pipelines::full_row(&state.db(), &id)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;

    let input_defs = serde_json::from_str::<serde_json::Value>(&r.input_defs).ok();

    // Fetch explicit secret bindings
    let secret_ids = crate::store::pipelines::secret_bind_ids(&state.db(), &id).await?;

    // Fetch explicit variable bindings
    let var_ids = crate::store::pipelines::var_bind_ids(&state.db(), &id).await?;

    // Fetch inline env vars (pipeline-level `env:` block) for the edit form.
    let env_rows = crate::store::pipelines::inline_envs(&state.db(), &id).await?;
    let envs: Vec<serde_json::Value> = env_rows
        .iter()
        .map(|(name, value)| {
            serde_json::json!({
                "name": name,
                "value": value,
            })
        })
        .collect();

    let tasks = crate::store::pipelines::tasks_rows(&state.db(), &id)
        .await?
        .into_iter()
        .map(|t| {
        serde_json::json!({
            "id": t.id,
            "step_order": t.step_order,
            "name": t.name,
            "exec_type": t.exec_type,
            "host_id": t.host_id,
            "host_names": t.host_names,
            "host_labels": t.host_labels,
            "command_template": t.command_template,
            "image": t.image,
            "work_dir": t.work_dir,
            "retry_count": t.retry_count,
            "timeout_seconds": t.timeout_seconds,
            "fail_strategy": t.fail_strategy,
            "volumes": serde_json::from_str::<serde_json::Value>(&t.volumes)
                .unwrap_or(serde_json::Value::Array(vec![])),
            "step_secrets": t.step_secrets,
            "step_env": serde_json::from_str::<serde_json::Value>(&t.step_env)
                .ok().filter(|v| v.is_object()),
            "privileged": t.privileged != 0,
            "config_render": serde_json::Value::Null,
            "config_dest": serde_json::Value::Null,
            "hosts_strategy": t.hosts_strategy,
            "capabilities": serde_json::from_str::<Vec<String>>(&t.capabilities).unwrap_or_default(),
            "needs": serde_json::from_str::<Vec<String>>(&t.needs_json).unwrap_or_default(),
        })
    })
    .collect::<Vec<_>>();

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.id,
        "name": r.name,
        "description": r.description,
        "trigger_type": r.trigger_type,
        "webhook_path": r.webhook_path,
        // Secret NAME only (never the value) — the form needs it for edits.
        "webhook_secret": r.webhook_secret,
        "cron_expr": r.cron_expr,
        "runner_labels": serde_json::from_str::<Vec<String>>(&r.runner_labels).unwrap_or_default(),
        "timeout_seconds": r.timeout_seconds,
        "source": r.source,
        "input_defs": input_defs,
        "concurrency_group": r.concurrency_group,
        "is_active": r.is_active != 0,
        "owner_group": r.owner_group,
        "team_id": r.team_id,
        "secret_ids": secret_ids,
        "var_ids": var_ids,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
        "concurrency_mode": r.concurrency_mode,
        "concurrency_max_queue": r.concurrency_max_queue,
        "concurrency_queue_timeout_seconds": r.concurrency_queue_timeout_seconds,
        "yaml_path": r.yaml_path,
        "environments": serde_json::from_str::<Vec<String>>(&r.environments).unwrap_or_default(),
        "notify": serde_json::from_str::<serde_json::Value>(&r.notify_json)
            .unwrap_or_else(|_| serde_json::json!([])),
        "tasks": tasks,
        "env": envs,
        "yaml_sync": crate::services::pipeline_scanner::yaml_sync_status(&state.pool, &id)
            .await
            .ok(),
    }))))
}

pub async fn update(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<CreatePipelineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let current_team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&current_team_id, Perm::PipelineWrite)?;

    // Form retirement: every pipeline is file-backed, and this endpoint
    // rewrites the pipeline's YAML file (see the write below), so there is no
    // DB-vs-disk split left to guard. The row check is what 404s a bad id.

    let before_facts = crate::store::pipelines::update_facts(&state.db(), &id).await?;
    let before = before_facts
        .as_ref()
        .map(|(name, trigger_type, team, owner, is_active)| {
            serde_json::json!({
                "id": id,
                "name": name,
                "trigger_type": trigger_type,
                "team_id": team,
                "owner_group": owner,
                "is_active": is_active,
            })
        });

    let (team_id, owner_group) = if req.team_id.is_some() || req.owner_group.is_some() {
        let resolved = resolve_required_team_id(
            &state.pool,
            req.team_id.as_deref(),
            req.owner_group.as_deref(),
        )
        .await?;
        auth.check_team(&resolved.0, Perm::PipelineWrite)?;
        resolved
    } else {
        let slug = crate::store::pipelines::owner_group_of(&state.db(), &id).await?;
        (current_team_id, slug)
    };

    verify_pipeline_secret_bindings(&state, &auth, &team_id, req.secret_ids.as_ref()).await?;
    super::environments::validate_environment_slugs(&state.db(), &req.environments).await?;
    let webhook_secret = validate_webhook_secret_binding(
        &state,
        &team_id,
        &req.trigger_type,
        req.webhook_secret.as_deref(),
    )
    .await?;

    let stored_team_id = crate::store::pipelines::team_of(&state.db(), &id).await?;
    if team_id != stored_team_id {
        let active_jobs = crate::store::pipelines::active_job_count(&state.db(), &id).await?;
        if active_jobs > 0 {
            return Err(AppError::Conflict(format!(
                "pipeline team cannot change while {active_jobs} active job(s) exist; finish or cancel them first"
            )));
        }
    }

    let labels_json = serde_json::to_string(&req.runner_labels).unwrap_or_else(|_| "[]".into());
    let input_defs_json = serde_json::to_string(&req.input_defs.clone().unwrap_or_default())
        .unwrap_or_else(|_| "[]".into());
    let environments_json =
        serde_json::to_string(&req.environments).unwrap_or_else(|_| "[]".into());
    let notify_json = serde_json::to_string(&req.notify).unwrap_or_else(|_| "[]".into());

    // Form retirement: updates also land as the pipeline's YAML file. Secret /
    // variable bindings resolve to names — YAML references names, not IDs.
    let mut secret_names: HashMap<String, String> = HashMap::new();
    for secret_id in req.secret_ids.iter().flatten() {
        if let Some(name) =
            crate::store::pipelines::secret_name_in_team(&state.db(), secret_id, &team_id).await?
        {
            secret_names.insert(secret_id.clone(), name);
        }
    }
    let mut var_names: HashMap<String, String> = HashMap::new();
    for var_id in req.var_ids.iter().flatten() {
        if let Some(name) =
            crate::store::variables::variable_name_optional(&state.db(), var_id).await?
        {
            var_names.insert(var_id.clone(), name);
        }
    }
    let yaml = crate::services::pipeline_yaml_emit::emit_from_request(
        &req,
        &owner_group,
        &secret_names,
        &var_names,
    )?;
    let affected = crate::store::pipelines::update_guarded(
        &state.db(),
        &id,
        team_id != stored_team_id,
        &crate::store::pipelines::UpdatePipeline {
            name: req.name.clone(),
            description: req.description.clone().unwrap_or_default(),
            trigger_type: req.trigger_type.clone(),
            webhook_path: req.webhook_path.clone(),
            webhook_secret: Some(webhook_secret.clone()),
            cron_expr: req.cron_expr.clone(),
            runner_labels: labels_json,
            timeout_seconds: req.timeout_seconds.unwrap_or(3600) as i64,
            concurrency_group: req.concurrency_group.clone(),
            concurrency_mode: req.concurrency_mode.clone().unwrap_or_default(),
            concurrency_max_queue: req.concurrency_max_queue.unwrap_or(3) as i64,
            concurrency_queue_timeout_seconds: req.concurrency_queue_timeout_seconds.unwrap_or(1800)
                as i64,
            owner_group: owner_group.clone(),
            team_id: team_id.clone(),
            input_defs: input_defs_json,
            environments: environments_json,
            notify_json,
        },
    )
    .await?;

    if affected == 0 {
        return Err(AppError::NotFound("pipeline not found".into()));
    }

    // Rewrite the pipeline's definition (the authority). Un-migrated form rows
    // get bound here too — the same path as the startup migration.
    let (source, yaml_path, _name) =
        crate::store::pipelines::pipeline_source_row(&state.db(), &id).await?;
    let filename = match source.as_str() {
        "yaml" => yaml_path
            .map(|path| crate::services::pipeline_scanner::normalize_yaml_path_key(&path))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                super::super::services::pipeline_scanner::filename_from_pipeline_name(&req.name)
            }),
        _ => super::super::services::pipeline_scanner::filename_from_pipeline_name(&req.name),
    };
    let bind = (source != "yaml").then(|| id.clone());
    crate::services::pipeline_scanner::write_and_upsert_yaml(
        &state.pool,
        &filename,
        &yaml,
        &crate::services::plugin_resolver::effective_local_dir(
            &state.config.storage.data_dir,
            &state.config.plugin_index.local_dir,
        )
        .map_err(|e| AppError::Internal(e.to_string()))?,
        bind.as_deref(),
        crate::services::platform_settings::effective(
            &state.db(),
            crate::services::platform_settings::KEY_PLUGIN_SYNC_TIMEOUT,
            &state.config.plugin_index.sync_timeout_seconds.to_string(),
        )
        .await
        .parse()
        .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS),
        None,
        false,
    )
    .await?;

    tracing::info!("Pipeline updated: {id} → {filename}");

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::PipelineUpdate)
            .resource("pipeline", &id, &req.name)
            .team(Some(team_id.clone()))
            .before(before)
            .after(Some(serde_json::json!({
                "id": id,
                "name": req.name,
                "trigger_type": req.trigger_type,
                "team_id": team_id,
                "owner_group": owner_group,
                "step_count": req.steps.len(),
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": req.name,
        "environments": req.environments,
        "file": filename,
        "steps": req.steps.iter().map(|step| serde_json::json!({
            "name": step.name,
            "engine": step.engine,
            "uses": step.uses,
        })).collect::<Vec<_>>(),
    }))))
}

pub async fn delete(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::PipelineDelete)?;

    let facts = crate::store::pipelines::delete_facts(&state.db(), &id).await?;
    let (name, before) = match facts {
        Some((name, trigger_type, team, owner_group, source, yaml_path)) => {
            let path = if source == "yaml" {
                yaml_path.clone()
            } else {
                None
            };
            (
                name.clone(),
                Some(serde_json::json!({
                    "id": id,
                    "name": name,
                    "trigger_type": trigger_type,
                    "team_id": team,
                    "owner_group": owner_group,
                    "source": source,
                    "yaml_path": &path,
                })),
            )
        }
        None => (id.clone(), None),
    };

    let deleted = crate::store::pipelines::delete(&state.db(), &id).await?;

    if !deleted {
        return Err(AppError::NotFound("pipeline not found".into()));
    }

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::PipelineDelete)
            .resource("pipeline", &id, &name)
            .team(Some(team_id))
            .before(before),
    )
    .await;

    tracing::info!("Pipeline deleted: {id}");
    Ok(Json(ApiResponse::success(())))
}

pub async fn toggle_active(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;

    let (name, current_active) = crate::store::pipelines::name_active_of(&state.db(), &id)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;

    let new_active = !current_active;

    crate::store::pipelines::set_active(&state.db(), &id, new_active).await?;

    let action = if new_active {
        "activated"
    } else {
        "deactivated"
    };
    tracing::info!("Pipeline {name} ({id}) {action}");

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::PipelineToggleActive)
            .resource("pipeline", &id, &name)
            .team(Some(team_id))
            .before(Some(serde_json::json!({ "is_active": current_active })))
            .after(Some(serde_json::json!({ "is_active": new_active }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "is_active": new_active
    }))))
}

#[derive(Debug, Default, Deserialize)]
pub struct DisablePipelineRequest {
    /// Also cancel every in-flight (pending / queued / running /
    /// waiting_approval) job of this pipeline.
    #[serde(default)]
    pub cancel_in_flight: bool,
}

/// Idempotent emergency stop: ensures the pipeline is disabled and, on
/// request, cancels every in-flight job in the same transaction. Unlike
/// `toggle-active`, repeating this call never re-enables the pipeline.
pub async fn disable(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    body: Option<Json<DisablePipelineRequest>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;

    let outcome =
        crate::store::pipelines::disable(&state.db(), &auth, &team_id, &id, req.cancel_in_flight)
            .await?;
    let name = outcome.name;
    let cancelled = outcome.cancelled;
    if !cancelled.is_empty() {
        crate::services::poll_events::wake_all();
    }

    // Post-commit fan-out per cancelled job: notify channels + WebSocket.
    for job_id in &cancelled {
        crate::services::transition::announce_job_status(&state, job_id, "cancelled", true).await;
    }

    let cancelled_ids: Vec<&str> = cancelled.iter().map(String::as_str).collect();

    tracing::info!(
        "Pipeline {name} ({id}) disabled (cancelled {} in-flight jobs)",
        cancelled.len()
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "is_active": false,
        "cancelled_count": cancelled.len(),
        "cancelled_jobs": cancelled_ids,
    }))))
}

/// Idempotent re-enable. No-op when already active; never touches jobs —
/// runs cancelled by `disable` stay cancelled.
pub async fn enable(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;

    let name = crate::store::pipelines::enable(&state.db(), &auth, &team_id, &id).await?;

    tracing::info!("Pipeline {name} ({id}) enabled");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "is_active": true
    }))))
}

pub async fn trigger(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<shared::protocol::TriggerPipelineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::PipelineTrigger)?;

    // Admin-only debug trigger: skips secret redaction in the Runner's logs for this job.
    // Non-admins are rejected outright; the flag is never honored silently.
    let debug_unredact = body.debug.unwrap_or(false);
    if debug_unredact && !auth.is_platform_admin {
        return Err(AppError::Forbidden(
            "debug (no-redaction) trigger requires a platform admin".into(),
        ));
    }

    // Verbose execution mode is available to everyone — it only adds detail,
    // it never reduces redaction.
    let verbose = body.verbose.unwrap_or(false);

    let result = do_trigger(
        &state,
        &id,
        &auth.username,
        &body.inputs,
        debug_unredact,
        verbose,
        body.environment.as_deref(),
        Some(auth.id.as_str()),
        Some(&auth),
    )
    .await?;
    let name = crate::store::pipelines::name_of(&state.db(), &id)
        .await?
        .unwrap_or_else(|| id.clone());
    let job_id = result
        .0
        .data
        .as_ref()
        .and_then(|d| d.get("job_id"))
        .cloned();

    let input_defs_raw = crate::store::pipelines::input_defs_of(&state.db(), &id)
        .await?
        .unwrap_or_else(|| "[]".into());
    let defs: Vec<PipelineInputDef> = serde_json::from_str(&input_defs_raw).unwrap_or_default();
    let masking_keys = masking_input_names(&defs);
    let audit_inputs = redact_input_map(&body.inputs, &masking_keys);

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::PipelineTrigger)
            .resource("pipeline", &id, &name)
            .team(Some(team_id))
            .after(Some(serde_json::json!({
                "job_id": job_id,
                "inputs": audit_inputs,
                "debug_unredact": debug_unredact,
                "verbose": verbose,
                "environment": body.environment,
            }))),
    )
    .await;
    Ok(result)
}

/// Shared trigger logic — used by admin trigger, webhooks, and cron scheduler.
///
/// `trigger_user_id` / `auth` are `Some` for manual UI triggers (ACL + self-approve);
/// automated sources (webhook / cron / git) pass `None`.
#[allow(clippy::too_many_arguments)]
pub async fn do_trigger(
    state: &AppState,
    pipeline_id: &str,
    triggered_by: &str,
    inputs: &std::collections::HashMap<String, String>,
    debug_unredact: bool,
    verbose: bool,
    environment: Option<&str>,
    trigger_user_id: Option<&str>,
    auth: Option<&AuthUser>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    // The stored definition is the snapshot source: no on-disk re-read. A
    // broken definition cannot be saved in the first place, so the previous
    // good projection is the current one.

    let pipeline = crate::store::pipelines::trigger_row(&state.db(), pipeline_id)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
    if pipeline.is_active == 0 {
        return Err(AppError::Conflict(
            "pipeline is disabled — enable it before triggering".into(),
        ));
    }

    let concurrency_group: Option<String> = pipeline.concurrency_group;
    let concurrency_mode: String = pipeline.concurrency_mode;
    let concurrency_max_queue: i64 = pipeline.concurrency_max_queue;
    let concurrency_queue_timeout: i64 = pipeline.concurrency_queue_timeout_seconds;
    let input_defs_raw: String = pipeline.input_defs;
    let team_id: String = pipeline.team_id;
    let declared_environments: Vec<String> =
        serde_json::from_str(&pipeline.environments).unwrap_or_default();
    // Resource profile requested by `runner.resources`. The authorization
    // subject is the job's resolved team (the same team used for dispatch and
    // runner binding). Unauthorized → fail fast before anything is scheduled.
    let resource_profile: String = pipeline.resource_profile.trim().to_string();
    crate::services::resource_grants::ensure_team_may_use(&state.db(), &team_id, &resource_profile)
        .await
        .inspect_err(|error| {
            tracing::warn!(
                pipeline = %pipeline_id,
                team = %team_id,
                profile = %resource_profile,
                %error,
                "trigger refused: team is not granted the requested resource profile"
            )
        })?;
    let input_defs: Vec<PipelineInputDef> =
        serde_json::from_str(&input_defs_raw).unwrap_or_default();
    let masking_keys = masking_input_names(&input_defs);

    // Merge declared defaults, then validate inputs against the pipeline's
    // declared constraints (type / pattern / choice / host / required) before
    // anything is scheduled. Fail fast with field-level errors so the UI can
    // point at the offending input.
    let mut resolved_inputs = inputs.clone();
    for def in &input_defs {
        let present = resolved_inputs
            .get(&def.name)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        if !present {
            if let Some(d) = def.default.as_deref() {
                resolved_inputs.insert(def.name.clone(), d.to_string());
            }
        }
    }
    // Host references are validated against the hosts registry. Load the
    // name set once up-front so the shared validator stays DB-agnostic.
    let host_names: std::collections::HashSet<String> =
        crate::store::pipelines::all_host_names(&state.db())
            .await?
            .into_iter()
            .collect();
    if let Err(field_errors) = validate_input_values(&input_defs, &resolved_inputs, |host_name| {
        host_names.contains(host_name)
    }) {
        let detail = field_errors
            .iter()
            .map(|e| format!("{}: {}", e.name, e.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AppError::ValidationFailed(format!(
            "invalid trigger inputs — {detail}"
        )));
    }
    let inputs = &resolved_inputs;

    let inputs_for_snapshot = redact_input_map(inputs, &masking_keys);

    // Effective mode frozen into the job snapshot (Job Detail). Empty mode with
    // a non-empty group matches legacy trigger behavior → discard-new.
    let effective_concurrency_mode: Option<String> = concurrency_group
        .as_ref()
        .filter(|g| !g.is_empty())
        .map(|_| {
            if concurrency_mode.is_empty() {
                "discard-new".to_string()
            } else {
                concurrency_mode.clone()
            }
        });

    // Multi-env governance: every run must explicitly pick one of the
    // pipeline's declared environments (GHA binds environment in code; buz
    // binds it in the pipeline definition and enforces at trigger time).
    let environment = if declared_environments.is_empty() {
        // Legacy unbound pipeline (pre-migration row) — keep old behavior.
        environment
    } else {
        let Some(env_slug) = environment.map(str::trim).filter(|s| !s.is_empty()) else {
            return Err(AppError::ValidationFailed(format!(
                "environment is required — pipeline targets {}",
                declared_environments.join(", ")
            )));
        };
        if !declared_environments.iter().any(|e| e == env_slug) {
            return Err(AppError::ValidationFailed(format!(
                "environment '{env_slug}' is not in this pipeline's declared environments [{}]",
                declared_environments.join(", ")
            )));
        }
        Some(env_slug)
    };

    let env_resolved = super::environments::resolve_environment(&state.db(), environment).await?;
    let (environment_id, environment_slug, env_protection) = match &env_resolved {
        Some(e) => (
            Some(e.id.as_str()),
            Some(e.slug.as_str()),
            Some(&e.protection),
        ),
        None => (None, None, None),
    };

    // ── Protection gate: waiting_approval skips concurrency until approved ──
    let needs_approval = env_protection.map(|p| p.enabled).unwrap_or(false);
    let job_status: &str = if needs_approval {
        let protection = env_protection.expect("checked above");
        match auth {
            None => {
                return Err(AppError::Forbidden(
                    "protected environment rejects automated triggers".into(),
                ));
            }
            Some(user) => {
                user.check_team_role(&team_id, protection.min_trigger_role)?;
            }
        }
        "waiting_approval"
    } else {
        let status = crate::store::pipelines::resolve_concurrency_status(
            &state.db(),
            pipeline_id,
            concurrency_group.as_deref(),
            &concurrency_mode,
            concurrency_max_queue,
        )
        .await?;
        status
    };

    // ── Freeze snapshot ──
    let job_id = Uuid::new_v4().to_string();
    let pipeline_name: String = pipeline.name.clone();
    let runner_labels: String = pipeline.runner_labels.clone();

    // Capture the full pipeline definition so historical runs are
    // reproducible even if the pipeline is later modified or deleted.
    let mut tasks_snapshot = crate::store::pipelines::task_snapshot_rows(&state.db(), pipeline_id)
        .await?
        .into_iter()
        .map(|t| {
        serde_json::json!({
            "step_order": t.step_order,
            "name": t.name,
            "exec_type": t.exec_type,
            "host_id": t.host_id,
            "host_names": t.host_names,
            "host_labels": t.host_labels,
            "command_template": t.command_template,
            "image": t.image,
            "work_dir": t.work_dir,
            "retry_count": t.retry_count,
            "timeout_seconds": t.timeout_seconds,
            "fail_strategy": t.fail_strategy,
            "step_secrets": t.step_secrets,
            "step_env": serde_json::from_str::<serde_json::Value>(&t.step_env)
                .ok().filter(|v| v.is_object()),
            "volumes": serde_json::from_str::<serde_json::Value>(&t.volumes)
                .unwrap_or(serde_json::Value::Array(vec![])),
            "privileged": t.privileged != 0,
            "config_render": serde_json::Value::Null,
            "config_dest": serde_json::Value::Null,
            "hosts_strategy": t.hosts_strategy,
            "id": t.id,
            "capabilities": serde_json::from_str::<Vec<String>>(&t.capabilities).unwrap_or_default(),
            "hosts_max_parallel": t.hosts_max_parallel,
            "needs": serde_json::from_str::<Vec<String>>(&t.needs_json)
                .unwrap_or_default(),
        })
    })
    .collect::<Vec<_>>();

    let resolutions = crate::store::pipelines::step_resolutions(&state.db(), pipeline_id).await?;
    for resolution in resolutions {
        if let Some(task) = tasks_snapshot
            .iter_mut()
            .find(|task| task["step_order"].as_i64() == Some(resolution.step_order))
        {
            task["marketplace"] = serde_json::json!({
                "uses_ref": resolution.uses_ref,
                "resolved_version": resolution.resolved_version,
                "content_sha256": resolution.content_sha256,
                "source": resolution.source,
                "reviewed": resolution.reviewed != 0,
                "inputs": serde_json::from_str::<serde_json::Value>(
                    &resolution.input_defs_json,
                ).unwrap_or(serde_json::json!([])),
            });
        }
    }

    // ── Environment host allowlist ──────────────────────────────────────────
    // A deployment to the chosen environment may only touch hosts the
    // environment declares (by name, or by host label). Empty allowlist =
    // unrestricted. Static `host:` / `host_labels:` declarations are checked
    // Static host declarations are checked at trigger time. Dynamic host
    // references resolve only when a Runner claims the job, so restricted
    // environments reject them rather than allowing an unchecked target.
    if let Some(env_id) = environment_id {
        let allowed = crate::store::pipelines::env_host_allowlist(&state.db(), env_id).await?;
        if !allowed.is_empty() {
            let allowed_names: std::collections::HashSet<&str> = allowed
                .iter()
                .filter(|(kind, _)| kind == "name")
                .map(|(_, v)| v.as_str())
                .collect();
            let allowed_labels: std::collections::HashSet<&str> = allowed
                .iter()
                .filter(|(kind, _)| kind == "label")
                .map(|(_, v)| v.as_str())
                .collect();
            // Preload host → labels once so label membership is checked in
            // memory (hosts.labels is a JSON string array).
            let host_label_sets: std::collections::HashMap<
                String,
                std::collections::HashSet<String>,
            > = crate::store::pipelines::host_label_rows(&state.db())
                .await?
                .into_iter()
                .map(|(name, labels_json)| {
                    let labels: Vec<String> =
                        serde_json::from_str(&labels_json).unwrap_or_default();
                    (name, labels.into_iter().collect())
                })
                .collect();
            let mut offenders: Vec<String> = Vec::new();
            for task in &tasks_snapshot {
                let step = task["name"].as_str().unwrap_or("unnamed");
                for name in task["host_names"]
                    .as_str()
                    .unwrap_or("")
                    .split(',')
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                {
                    if name.contains('$') {
                        offenders.push(format!("step '{step}' → dynamic host '{name}'"));
                        continue;
                    }
                    let ok = allowed_names.contains(name)
                        || host_label_sets.get(name).is_some_and(|host_labels| {
                            host_labels
                                .iter()
                                .any(|host_label| allowed_labels.contains(host_label.as_str()))
                        });
                    if !ok {
                        offenders.push(format!("step '{step}' → host '{name}'"));
                    }
                }
                for label in task["host_labels"]
                    .as_str()
                    .unwrap_or("")
                    .split(',')
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                {
                    if label.contains('$') {
                        offenders.push(format!("step '{step}' → dynamic host label '{label}'"));
                    } else if !allowed_labels.contains(label) {
                        offenders.push(format!("step '{step}' → host label '{label}'"));
                    }
                }
            }
            if !offenders.is_empty() {
                return Err(AppError::ValidationFailed(format!(
                    "environment '{}' host allowlist rejects: {}",
                    environment_slug.unwrap_or("default"),
                    offenders.join("; ")
                )));
            }
        }
    }

    let env_snapshot = crate::store::pipelines::pipeline_var_flags(&state.db(), pipeline_id)
        .await?
        .into_iter()
        .map(|v| {
            serde_json::json!({
                "name": v.name,
                "is_secret": v.is_secret != 0,
            })
        })
        .collect::<Vec<_>>();

    let pipeline_snapshot = crate::store::pipelines::trigger_snapshot_row(&state.db(), pipeline_id)
        .await?
        .map(|p| {
            serde_json::json!({
                "trigger_type": p.trigger_type,
                "timeout_seconds": p.timeout_seconds,
                "source": p.source,
            })
        })
        .unwrap_or(serde_json::json!({}));

    // Freeze notify config (keep `${secrets.…}`; resolve at send). The column
    // is projected from the stored definition, so it is the single source.
    let notify_snapshot = {
        let raw = crate::store::pipelines::notify_json_of(&state.db(), pipeline_id).await?;
        let channels: Vec<shared::pipeline_yaml::NotifyChannelYaml> = raw
            .as_deref()
            .filter(|raw| !raw.trim().is_empty())
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();
        if channels.is_empty() {
            None
        } else {
            Some(serde_json::to_value(&channels).unwrap_or_default())
        }
    };

    let mut snapshot = serde_json::json!({
        "pipeline": pipeline_snapshot,
        "team_id": team_id,
        "runner_labels": serde_json::from_str::<Vec<String>>(&runner_labels).unwrap_or_default(),
        "resource_profile": resource_profile,
        "concurrency_group": concurrency_group,
        "concurrency_mode": effective_concurrency_mode,
        "concurrency_max_queue": concurrency_group
            .as_ref()
            .filter(|g| !g.is_empty())
            .map(|_| concurrency_max_queue),
        "concurrency_queue_timeout_seconds": concurrency_group
            .as_ref()
            .filter(|g| !g.is_empty())
            .map(|_| concurrency_queue_timeout),
        "tasks": tasks_snapshot,
        "env": env_snapshot,
        "inputs": inputs_for_snapshot,
        "masking_input_keys": masking_keys,
        "triggered_by": triggered_by,
        "triggered_at": chrono::Utc::now().to_rfc3339(),
        "notify": notify_snapshot,
        "environment": environment_slug,
    });
    if let Some(protection) = env_protection {
        if let Some(map) = snapshot.as_object_mut() {
            map.insert("protection".into(), protection.to_snapshot_json());
        }
    }
    if job_status == "waiting_approval" {
        if let Some(map) = snapshot.as_object_mut() {
            map.insert(
                "approval".into(),
                serde_json::json!({ "required": true, "received": 0 }),
            );
        }
    }

    // Masking input values encrypted at rest (buzenc:v1:…); claim decrypts for Runner.
    let sealed = crypto::seal_trigger_params(inputs, &masking_keys, &state.config.auth.jwt_secret)
        .map_err(AppError::Internal)?;
    let trigger_params_json = serde_json::to_string(&sealed).unwrap_or_else(|_| "{}".into());

    // Allocate the GHA-style build number and create the job atomically (see
    // `trigger_reserve::reserve_job` for the invariants).
    crate::store::trigger_reserve::reserve_job(
        &state.db(),
        &crate::store::trigger_reserve::ReservedJob {
            job_id: &job_id,
            pipeline_id,
            status: job_status,
            triggered_by,
            trigger_params_json: &trigger_params_json,
            snapshot: &snapshot,
            debug_unredact,
            verbose,
            environment_id,
            trigger_user_id,
            resource_profile: &resource_profile,
        },
    )
    .await?;
    crate::services::poll_events::wake_all();

    tracing::info!("Pipeline triggered: {pipeline_name} → job {job_id} (by: {triggered_by})");

    // Git adapter records a richer TriggerEvent itself; skip duplicate here.
    if triggered_by != "git" {
        let source = match triggered_by {
            "webhook" => "webhook",
            "cron" => "cron",
            _ => "manual",
        };
        crate::store::trigger_events::record_simple_trigger(
            &state.db(),
            pipeline_id,
            source,
            triggered_by,
            &job_id,
        )
        .await;
    }

    let mut resp = serde_json::json!({
        "job_id": job_id,
        "pipeline_id": pipeline_id,
        "pipeline_name": pipeline_name,
        "status": job_status,
    });
    if job_status == "waiting_approval" {
        if let Some(map) = resp.as_object_mut() {
            map.insert("approval_required".into(), serde_json::json!(true));
        }
    }

    Ok(Json(ApiResponse::success(resp)))
}

// --- helpers ---

/// Webhook pipelines must bind `webhook_secret` — the NAME of a team secret
/// whose value signs every hook request (`X-Buz-Signature`). An unprotected
/// webhook is triggerable by anyone who can guess the path, so an empty name
/// is rejected outright and a name with no matching team secret is rejected
/// so the pipeline never goes live un-signable. Returns the stored name
/// (empty for non-webhook triggers).
async fn validate_webhook_secret_binding(
    state: &AppState,
    team_id: &str,
    trigger_type: &str,
    webhook_secret: Option<&str>,
) -> Result<String, AppError> {
    if trigger_type != "webhook" {
        return Ok(String::new());
    }
    let name = webhook_secret.unwrap_or("").trim();
    if name.is_empty() {
        tracing::warn!(
            team = %team_id,
            "webhook pipeline rejected: webhook_secret is mandatory"
        );
        return Err(AppError::ValidationFailed(
            "webhook pipelines require `webhook_secret` — the name of a team secret whose value signs hook requests (X-Buz-Signature)"
                .into(),
        ));
    }
    let exists =
        crate::store::pipelines::team_secret_id_by_name(&state.db(), team_id, name).await?;
    if exists.is_none() {
        tracing::warn!(
            team = %team_id,
            secret = %name,
            "webhook pipeline rejected: webhook_secret names no secret in this team"
        );
        return Err(AppError::ValidationFailed(format!(
            "webhook_secret '{name}' does not match any secret in this team — create the secret first"
        )));
    }
    Ok(name.to_string())
}

async fn verify_pipeline_secret_bindings(
    state: &AppState,
    auth: &AuthUser,
    team_id: &str,
    secret_ids: Option<&Vec<String>>,
) -> Result<(), AppError> {
    let Some(ids) = secret_ids.filter(|v| !v.is_empty()) else {
        return Ok(());
    };
    auth.check_team(team_id, Perm::SecretManage)?;
    for secret_id in ids {
        let secret_team = secret_team_id(&state.pool, secret_id).await?;
        if secret_team != team_id {
            return Err(AppError::ValidationFailed(format!(
                "secret '{secret_id}' belongs to a different team"
            )));
        }
    }
    Ok(())
}

pub(crate) async fn insert_steps(
    conn: &mut sqlx::SqliteConnection,
    pipeline_id: &str,
    steps: &[StepDef],
) -> Result<Vec<serde_json::Value>, AppError> {
    let mut result = Vec::with_capacity(steps.len());

    for (i, step) in steps.iter().enumerate() {
        // Step definitions must already be expanded before they are persisted.
        if step.uses.is_some() {
            return Err(AppError::ValidationFailed(format!(
                "step '{}' uses marketplace reference '{}' — step resolution is not \
                 available in this build yet",
                step.name,
                step.uses.as_deref().unwrap_or_default()
            )));
        }
        if step.run.trim().is_empty() {
            return Err(AppError::ValidationFailed(format!(
                "step '{}' has no 'run' command",
                step.name
            )));
        }
        let task_id = Uuid::new_v4().to_string();
        let exec_type = step.engine.clone().unwrap_or_else(|| "shell".into());
        shared::plugin_spec::validate_plugin_capabilities(&step.capabilities, &exec_type)
            .map_err(AppError::ValidationFailed)?;
        let capabilities_stored = serde_json::to_string(&step.capabilities)
            .map_err(|error| AppError::Internal(error.to_string()))?;
        let fail_strategy = if step.continue_on_error.unwrap_or(false) {
            "continue"
        } else {
            "stop"
        };

        // ── Host names: store raw values, resolve static ones to IDs ──
        let raw_host_names: Vec<String> =
            step.host.as_ref().map(|h| h.to_vec()).unwrap_or_default();
        let host_names_stored = raw_host_names.join(",");

        // ── Host resolution (only for SSH steps) ──
        let host_ids = if exec_type == "ssh" {
            let mut ids: Vec<String> = Vec::new();

            // 1) Explicit host names — skip those containing '$' (dynamic references)
            for name in &raw_host_names {
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                if name.contains('$') {
                    // Dynamic reference — defer resolution to poll time
                    tracing::debug!(
                        "Step '{}' has dynamic host reference '{}' — deferring resolution",
                        step.name,
                        name
                    );
                    continue;
                }
                if let Ok(Some(host_id)) =
                    crate::store::pipelines::host_id_by_name(&mut *conn, name).await
                {
                    ids.push(host_id);
                } else {
                    tracing::warn!(
                        "Step '{}' references unknown host '{}' — will fail at dispatch",
                        step.name,
                        name
                    );
                }
            }

            // 2) Host labels — skip those containing '$'
            if let Some(ref labels) = step.host_labels {
                for label in &labels.to_vec() {
                    let label = label.trim();
                    if label.is_empty() {
                        continue;
                    }
                    if label.contains('$') {
                        tracing::debug!(
                            "Step '{}' has dynamic host label '{}' — deferring resolution",
                            step.name,
                            label
                        );
                        continue;
                    }
                    match crate::store::pipelines::host_ids_by_label(&mut *conn, label).await {
                        Ok(rows) => {
                            if rows.is_empty() {
                                tracing::warn!(
                                    "Step '{}': label '{}' matched no hosts",
                                    step.name,
                                    label
                                );
                            }
                            ids.extend(rows);
                        }
                        Err(e) => {
                            tracing::error!(
                                "Step '{}': failed to query hosts by label '{}': {e}",
                                step.name,
                                label
                            );
                        }
                    }
                }
            }

            // Deduplicate while preserving order
            let mut seen = std::collections::HashSet::new();
            ids.retain(|id| seen.insert(id.clone()));
            Some(ids.join(","))
        } else {
            None
        };
        // Foreign key: bind NULL when no host is resolved (empty string violates REFERENCES).
        let host_id_bind: Option<String> = host_ids.filter(|s| !s.is_empty());
        // Normalize host_labels to a comma-separated string for storage
        let host_labels_stored = step
            .host_labels
            .as_ref()
            .map(|l| l.join(","))
            .unwrap_or_default();
        let hosts_strategy_stored =
            shared::pipeline_yaml::normalize_hosts_strategy(step.hosts_strategy.as_deref())
                .unwrap_or("chain")
                .to_string();
        // Step-level secret allowlist (comma-separated)
        let step_secrets_stored = step
            .secrets
            .as_ref()
            .map(|s| s.join(","))
            .unwrap_or_default();
        // Step-level env map JSON (values may contain ${secrets.X} / ${vars.X})
        let step_env_stored = step
            .env
            .as_ref()
            .filter(|m| !m.is_empty())
            .map(|m| serde_json::to_string(m).unwrap_or_default())
            .unwrap_or_default();
        let image_stored = step
            .image
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .to_string();

        let volumes_stored = step
            .volumes
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default())
            .unwrap_or_default();

        let privileged_val = if step.privileged.unwrap_or(false) {
            1i64
        } else {
            0i64
        };

        crate::store::pipelines::insert_task_tx(
            conn,
            &crate::store::pipelines::TaskInsert {
                id: task_id.clone(),
                pipeline_id: pipeline_id.to_string(),
                step_order: i as i64,
                name: step.name.clone(),
                exec_type: exec_type.clone(),
                host_id: host_id_bind.clone(),
                host_names: host_names_stored.clone(),
                host_labels: host_labels_stored.clone(),
                command_template: step.run.clone(),
                image: image_stored.clone(),
                work_dir: step.dir.clone().unwrap_or_default(),
                retry_count: step.retry.unwrap_or(0) as i64,
                timeout_seconds: parse_timeout_seconds(step.timeout.as_deref()),
                fail_strategy: fail_strategy.to_string(),
                step_secrets: step_secrets_stored.clone(),
                step_env: step_env_stored.clone(),
                volumes: volumes_stored.clone(),
                privileged: privileged_val,
                hosts_strategy: hosts_strategy_stored.clone(),
                capabilities: capabilities_stored.clone(),
                hosts_max_parallel: step.hosts_max_parallel.map(|v| v as i64),
                needs_json: serde_json::to_string(&step.needs).unwrap_or_else(|_| "[]".into()),
            },
        )
        .await?;

        result.push(serde_json::json!({
            "id": task_id,
            "step_order": i,
            "name": step.name,
            "exec_type": exec_type,
            "image": image_stored,
            "capabilities": step.capabilities,
        }));
    }

    Ok(result)
}

fn parse_timeout_seconds(raw: Option<&str>) -> i64 {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => 3600,
        Some(s) => shared::pipeline_yaml::parse_duration_seconds(s).unwrap_or(3600),
    }
}

pub(crate) async fn insert_env_vars(
    conn: &mut sqlx::SqliteConnection,
    pipeline_id: &str,
    envs: &[shared::protocol::EnvPair],
) -> Result<(), AppError> {
    for env in envs {
        crate::store::pipelines::insert_inline_env_tx(conn, pipeline_id, &env.name, &env.value)
            .await?;
    }
    Ok(())
}

/// Explicitly bind variable IDs to a pipeline (many-to-many via pipeline_vars).
pub(crate) async fn insert_pipeline_var_bindings(
    conn: &mut sqlx::SqliteConnection,
    pipeline_id: &str,
    var_ids: &[String],
) -> Result<(), AppError> {
    for var_id in var_ids {
        crate::store::pipelines::insert_var_binding_tx(conn, pipeline_id, var_id).await?;
    }
    Ok(())
}

/// Explicitly bind secret IDs to a pipeline (many-to-many via pipeline_secrets).
pub(crate) async fn insert_pipeline_secret_bindings(
    conn: &mut sqlx::SqliteConnection,
    pipeline_id: &str,
    secret_ids: &[String],
) -> Result<(), AppError> {
    for secret_id in secret_ids {
        crate::store::pipelines::insert_secret_binding_tx(conn, pipeline_id, secret_id).await?;
    }
    Ok(())
}

// ── YAML validation ──

#[derive(serde::Deserialize)]
pub struct ValidateYamlRequest {
    pub yaml: String,
}

pub async fn validate_yaml(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<ValidateYamlRequest>,
) -> Result<Json<ApiResponse<shared::pipeline_yaml::YamlValidationResult>>, AppError> {
    // Viewer-scoped: no TeamView on YAML `team:` → generic error (no secret oracle).
    // Plugin policy mirrors what save will apply: runtime overrides win over
    // manager.toml, and the reviewed manifest falls back to the data root.
    let overrides = crate::services::platform_settings::fetch_overrides(&state.db()).await;
    let local_dir_setting = overrides
        .get(crate::services::platform_settings::KEY_PLUGIN_INDEX_LOCAL_DIR)
        .map(String::as_str)
        .unwrap_or(&state.config.plugin_index.local_dir);
    let policy = crate::services::yaml_validate::PluginPolicy {
        local_dir: crate::services::plugin_resolver::effective_local_dir(
            &state.config.storage.data_dir,
            local_dir_setting,
        )
        .map_err(|e| AppError::Internal(e.to_string()))?,
        reviewed_manifest: crate::services::plugin_resolver::effective_reviewed_manifest(
            &state.config.storage.data_dir,
            &state.config.plugin_index.reviewed_manifest,
            overrides
                .get(crate::services::platform_settings::KEY_PLUGIN_REVIEWED_MANIFEST)
                .map(String::as_str),
        ),
        require_reviewed: overrides
            .get(crate::services::platform_settings::KEY_PLUGIN_REVIEW_REQUIRED)
            .and_then(|value| value.parse::<bool>().ok())
            .unwrap_or(state.config.plugin_index.require_reviewed),
    };
    let result = crate::services::yaml_validate::validate_pipeline_yaml_for_viewer(
        &state.pool,
        &req.yaml,
        &auth,
        &policy,
    )
    .await?;
    Ok(Json(ApiResponse::success(result)))
}
