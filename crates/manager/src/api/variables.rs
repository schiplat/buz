use axum::{
    extract::{Path, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateVariableRequest, UpdateVariableRequest};
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent};
use crate::middleware::{AuthUser, PlatformAdmin};
use crate::state::AppState;

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = crate::store::variables::list_all(&state.pool).await?;

    let var_ids: Vec<String> = rows.iter().map(|(id, _, _, _)| id.clone()).collect();
    let binds_map = super::github_sync::variable_binds_map(&state.pool, &var_ids).await?;

    let vars: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, name, value, updated_at)| {
            // Values are frequently credentials — only platform admins read
            // them; everyone else gets metadata (names drive autocomplete).
            let value = if auth.is_platform_admin {
                serde_json::json!(value)
            } else {
                serde_json::Value::Null
            };
            let github_binds = binds_map.get(&id).cloned().unwrap_or_default();
            serde_json::json!({
                "id": id,
                "name": name,
                "value": value,
                "updated_at": updated_at,
                "github_binds": github_binds,
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(vars)))
}

pub async fn create(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<CreateVariableRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::ValidationFailed("name is required".into()));
    }

    // Check for duplicate
    if crate::store::variables::find_id_by_name(&state.pool, &name)
        .await?
        .is_some()
    {
        return Err(AppError::Conflict(format!(
            "variable '{name}' already exists"
        )));
    }

    let id = Uuid::new_v4().to_string();
    crate::store::variables::insert(&state.pool, &id, &name, &req.value).await?;

    tracing::info!("Variable created: {name} ({id})");

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::VariableCreate)
            .resource("variable", &id, &name)
            .after(Some(
                serde_json::json!({ "id": id, "name": name, "value": req.value }),
            )),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": name,
    }))))
}

pub async fn get_one(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let (row_id, name, stored_value, updated_at) =
        crate::store::variables::find_by_id(&state.pool, &id)
            .await?
            .ok_or_else(|| AppError::NotFound("variable not found".into()))?;

    // Same confidentiality rule as list(): values are admin-only.
    let value = if auth.is_platform_admin {
        serde_json::json!(stored_value)
    } else {
        serde_json::Value::Null
    };

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": row_id,
        "name": name,
        "value": value,
        "updated_at": updated_at,
    }))))
}

pub async fn update(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateVariableRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let (name, old_value) = crate::store::variables::name_value(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("variable not found".into()))?;
    crate::store::variables::update_value(&state.pool, &id, &req.value).await?;

    // Dirty-mark inherit GitHub binds when the platform default value changed.
    let mut github_binds_stale: u64 = 0;
    if old_value != req.value {
        github_binds_stale =
            super::github_sync::mark_variable_binds_stale_on_default_change(&state.pool, &id)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        var_id = %id,
                        error = %e,
                        "github-sync: failed to mark variable binds stale"
                    );
                    0
                });
    }

    tracing::info!("Variable updated: {name} ({id})");

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::VariableUpdate)
            .resource("variable", &id, &name)
            .before(Some(
                serde_json::json!({ "id": id, "name": name, "value": old_value }),
            ))
            .after(Some(
                serde_json::json!({ "id": id, "name": name, "value": req.value }),
            )),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "updated": true,
        "github_binds_stale": github_binds_stale,
    }))))
}

pub async fn delete(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let (name, old_value) = crate::store::variables::name_value(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("variable not found".into()))?;
    crate::store::variables::delete(&state.pool, &id).await?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::VariableDelete)
            .resource("variable", &id, &name)
            .before(Some(
                serde_json::json!({ "id": id, "name": name, "value": old_value }),
            )),
    )
    .await;

    tracing::info!("Variable deleted: {name} ({id})");
    Ok(Json(ApiResponse::success(())))
}
