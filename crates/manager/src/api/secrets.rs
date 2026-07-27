use axum::{Json, extract::{State, Path}};
use shared::protocol::{CreateSecretRequest, UpdateSecretRequest};
use shared::protocol::response::ApiResponse;
use shared::error::AppError;
use sqlx::Row;
use uuid::Uuid;

use crate::api::AppState;
use crate::middleware::AdminClaims;

pub async fn list(
    _auth: AdminClaims,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = sqlx::query(
        "SELECT id, name, value, updated_at FROM secrets ORDER BY name ASC",
    )
    .fetch_all(&state.pool)
    .await?;

    let secrets: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let raw = r.get::<String, _>(2);
            let masked = if raw.len() > 4 {
                format!("{}****{}", &raw[..2], &raw[raw.len() - 2..])
            } else {
                "****".to_string()
            };
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "name": r.get::<String, _>(1),
                "value": masked,
                "updated_at": r.get::<Option<String>, _>(3),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(secrets)))
}

pub async fn create(
    _auth: AdminClaims,
    State(state): State<AppState>,
    Json(req): Json<CreateSecretRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::ValidationFailed("name is required".into()));
    }

    // Check for duplicate
    let existing = sqlx::query("SELECT id FROM secrets WHERE name = ?")
        .bind(&name)
        .fetch_optional(&state.pool)
        .await?;
    if existing.is_some() {
        return Err(AppError::Conflict(format!("secret '{name}' already exists")));
    }

    let id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO secrets (id, name, value) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(&name)
        .bind(&req.value)
        .execute(&state.pool)
        .await?;

    tracing::info!("Secret created: {name} ({id})");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": name,
    }))))
}

pub async fn get_one(
    _auth: AdminClaims,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let r = sqlx::query("SELECT id, name, value, updated_at FROM secrets WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("secret not found".into()))?;

    let raw = r.get::<String, _>(2);
    let masked = if raw.len() > 4 {
        format!("{}****{}", &raw[..2], &raw[raw.len() - 2..])
    } else {
        "****".to_string()
    };

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.get::<String, _>(0),
        "name": r.get::<String, _>(1),
        "value": masked,
        "updated_at": r.get::<Option<String>, _>(3),
    }))))
}

pub async fn update(
    _auth: AdminClaims,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateSecretRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let existing = sqlx::query("SELECT name FROM secrets WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("secret not found".into()))?;

    let name: String = existing.get(0);
    sqlx::query("UPDATE secrets SET value = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(&req.value)
        .bind(&id)
        .execute(&state.pool)
        .await?;

    tracing::info!("Secret updated: {name} ({id})");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "updated": true,
    }))))
}

pub async fn delete(
    _auth: AdminClaims,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let existing = sqlx::query("SELECT name FROM secrets WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("secret not found".into()))?;

    let name: String = existing.get(0);
    sqlx::query("DELETE FROM secrets WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    tracing::info!("Secret deleted: {name} ({id})");
    Ok(Json(ApiResponse::success(())))
}
