use axum::{
    extract::{Path, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateSecretRequest, UpdateSecretRequest};
use shared::rbac::Perm;
use sqlx::Row;
use uuid::Uuid;

use crate::middleware::AuthUser;
use crate::rbac::{resolve_required_team_id, secret_team_id, team_in_clause};
use crate::state::AppState;

fn mask_value(raw: &str) -> String {
    if raw.len() > 4 {
        format!("{}****{}", &raw[..2], &raw[raw.len() - 2..])
    } else {
        "****".to_string()
    }
}

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let team_filter = team_in_clause(&auth, "team_id");
    let sql = match &team_filter {
        None => "SELECT id, name, value, owner_group, team_id, updated_at FROM secrets ORDER BY name ASC".to_string(),
        Some((clause, _)) if clause == "AND 1=0" => {
            return Ok(Json(ApiResponse::success(vec![])));
        }
        Some((clause, _)) => format!(
            "SELECT id, name, value, owner_group, team_id, updated_at FROM secrets WHERE 1=1 {clause} ORDER BY name ASC"
        ),
    };

    let mut q = sqlx::query(&sql);
    if let Some((_, ids)) = &team_filter {
        for id in ids {
            q = q.bind(id);
        }
    }

    let rows = q.fetch_all(&state.pool).await?;
    let secrets: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "name": r.get::<String, _>(1),
                "value": mask_value(&r.get::<String, _>(2)),
                "owner_group": r.get::<String, _>(3),
                "team_id": r.get::<String, _>(4),
                "updated_at": r.get::<Option<String>, _>(5),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(secrets)))
}

pub async fn create(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateSecretRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::ValidationFailed("name is required".into()));
    }

    let (team_id, slug) = resolve_required_team_id(
        &state.pool,
        req.team_id.as_deref(),
        req.owner_group.as_deref(),
    )
    .await?;
    auth.check_team(&team_id, Perm::SecretManage)?;

    let existing = sqlx::query("SELECT id FROM secrets WHERE name = ?")
        .bind(&name)
        .fetch_optional(&state.pool)
        .await?;
    if existing.is_some() {
        return Err(AppError::Conflict(format!(
            "secret '{name}' already exists"
        )));
    }

    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO secrets (id, name, value, owner_group, team_id) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&req.value)
    .bind(&slug)
    .bind(&team_id)
    .execute(&state.pool)
    .await?;

    tracing::info!("Secret created: {name} ({id}) team={slug}");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": name,
        "owner_group": slug,
        "team_id": team_id,
    }))))
}

pub async fn get_one(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = secret_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::TeamView)?;

    let r = sqlx::query(
        "SELECT id, name, value, owner_group, team_id, updated_at FROM secrets WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("secret not found".into()))?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.get::<String, _>(0),
        "name": r.get::<String, _>(1),
        "value": mask_value(&r.get::<String, _>(2)),
        "owner_group": r.get::<String, _>(3),
        "team_id": r.get::<String, _>(4),
        "updated_at": r.get::<Option<String>, _>(5),
    }))))
}

pub async fn update(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateSecretRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = secret_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::SecretManage)?;

    let (new_team_id, slug) = if req.team_id.is_some() || req.owner_group.is_some() {
        let resolved = resolve_required_team_id(
            &state.pool,
            req.team_id.as_deref(),
            req.owner_group.as_deref(),
        )
        .await?;
        auth.check_team(&resolved.0, Perm::SecretManage)?;
        resolved
    } else {
        let slug: String = sqlx::query_scalar("SELECT owner_group FROM secrets WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await?;
        (team_id, slug)
    };

    sqlx::query(
        "UPDATE secrets SET value = ?, owner_group = ?, team_id = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(&req.value)
    .bind(&slug)
    .bind(&new_team_id)
    .bind(&id)
    .execute(&state.pool)
    .await?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "updated": true,
        "team_id": new_team_id,
    }))))
}

pub async fn delete(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let team_id = secret_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::SecretManage)?;

    let name: String = sqlx::query_scalar("SELECT name FROM secrets WHERE id = ?")
        .bind(&id)
        .fetch_one(&state.pool)
        .await?;

    sqlx::query("DELETE FROM secrets WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    tracing::info!("Secret deleted: {name} ({id})");
    Ok(Json(ApiResponse::success(())))
}
