use axum::{
    extract::{Path, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateVariableRequest, UpdateVariableRequest};
use sqlx::Row;
use uuid::Uuid;

use crate::middleware::{AuthUser, PlatformAdmin};
use crate::state::AppState;

pub async fn list(
    _auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows =
        sqlx::query("SELECT id, name, value, updated_at FROM platform_vars ORDER BY name ASC")
            .fetch_all(&state.pool)
            .await?;

    let vars: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "name": r.get::<String, _>(1),
                "value": r.get::<String, _>(2),
                "updated_at": r.get::<Option<String>, _>(3),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(vars)))
}

pub async fn create(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<CreateVariableRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::ValidationFailed("name is required".into()));
    }

    // Check for duplicate
    let existing = sqlx::query("SELECT id FROM platform_vars WHERE name = ?")
        .bind(&name)
        .fetch_optional(&state.pool)
        .await?;
    if existing.is_some() {
        return Err(AppError::Conflict(format!(
            "variable '{name}' already exists"
        )));
    }

    let id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO platform_vars (id, name, value) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(&name)
        .bind(&req.value)
        .execute(&state.pool)
        .await?;

    tracing::info!("Variable created: {name} ({id})");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": name,
    }))))
}

pub async fn get_one(
    _auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let r = sqlx::query("SELECT id, name, value, updated_at FROM platform_vars WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("variable not found".into()))?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.get::<String, _>(0),
        "name": r.get::<String, _>(1),
        "value": r.get::<String, _>(2),
        "updated_at": r.get::<Option<String>, _>(3),
    }))))
}

pub async fn update(
    _auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateVariableRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let existing = sqlx::query("SELECT name FROM platform_vars WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("variable not found".into()))?;

    let name: String = existing.get(0);
    sqlx::query("UPDATE platform_vars SET value = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(&req.value)
        .bind(&id)
        .execute(&state.pool)
        .await?;

    tracing::info!("Variable updated: {name} ({id})");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "updated": true,
    }))))
}

pub async fn delete(
    _auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let existing = sqlx::query("SELECT name FROM platform_vars WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("variable not found".into()))?;

    let name: String = existing.get(0);
    sqlx::query("DELETE FROM platform_vars WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    tracing::info!("Variable deleted: {name} ({id})");
    Ok(Json(ApiResponse::success(())))
}
