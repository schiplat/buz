use axum::{
    extract::{Path, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateUserRequest, UpdateUserRequest};
use sqlx::Row;
use uuid::Uuid;

use crate::middleware::PlatformAdmin;
use crate::password::hash_password;
use crate::state::AppState;

pub async fn list(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = sqlx::query(
        "SELECT id, username, display_name, is_platform_admin, is_active, created_at, updated_at
         FROM users ORDER BY username ASC",
    )
    .fetch_all(&state.pool)
    .await?;

    let users = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "username": r.get::<String, _>(1),
                "display_name": r.get::<String, _>(2),
                "is_platform_admin": r.get::<i64, _>(3) != 0,
                "is_active": r.get::<i64, _>(4) != 0,
                "created_at": r.get::<Option<String>, _>(5),
                "updated_at": r.get::<Option<String>, _>(6),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(users)))
}

pub async fn create(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<CreateUserRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let username = req.username.trim();
    if username.is_empty() || req.password.is_empty() {
        return Err(AppError::ValidationFailed(
            "username and password are required".into(),
        ));
    }

    let id = Uuid::new_v4().to_string();
    let hash = hash_password(&req.password).map_err(AppError::Internal)?;
    let is_admin = req.is_platform_admin.unwrap_or(false);
    let display = req.display_name.unwrap_or_else(|| username.to_string());

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_active)
         VALUES (?, ?, ?, ?, ?, 1)",
    )
    .bind(&id)
    .bind(username)
    .bind(&hash)
    .bind(&display)
    .bind(if is_admin { 1 } else { 0 })
    .execute(&state.pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::Conflict("username already exists".into())
        } else {
            AppError::DatabaseError(e.to_string())
        }
    })?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "username": username,
        "is_platform_admin": is_admin,
    }))))
}

pub async fn update(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM users WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?;
    if exists.is_none() {
        return Err(AppError::NotFound("user not found".into()));
    }

    if let Some(ref pw) = req.password {
        if pw.is_empty() {
            return Err(AppError::ValidationFailed(
                "password cannot be empty".into(),
            ));
        }
        let hash = hash_password(pw).map_err(AppError::Internal)?;
        sqlx::query(
            "UPDATE users SET password_hash = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&hash)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }

    if let Some(ref dn) = req.display_name {
        sqlx::query("UPDATE users SET display_name = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(dn)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    if let Some(is_admin) = req.is_platform_admin {
        // Prevent removing the last platform admin
        if !is_admin && id == auth.id {
            let admin_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM users WHERE is_platform_admin = 1 AND is_active = 1",
            )
            .fetch_one(&state.pool)
            .await?;
            if admin_count <= 1 {
                return Err(AppError::ValidationFailed(
                    "cannot remove the last platform admin".into(),
                ));
            }
        }
        sqlx::query(
            "UPDATE users SET is_platform_admin = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(if is_admin { 1 } else { 0 })
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }

    if let Some(active) = req.is_active {
        if !active && id == auth.id {
            return Err(AppError::ValidationFailed(
                "cannot deactivate yourself".into(),
            ));
        }
        sqlx::query("UPDATE users SET is_active = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(if active { 1 } else { 0 })
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    Ok(Json(ApiResponse::success(serde_json::json!({ "id": id }))))
}

pub async fn delete(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    if id == auth.id {
        return Err(AppError::ValidationFailed("cannot delete yourself".into()));
    }
    let result = sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("user not found".into()));
    }
    Ok(Json(ApiResponse::success(())))
}
