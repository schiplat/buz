use axum::{
    extract::{Path, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateUserRequest, UpdateUserRequest};
use sqlx::Row;
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent};
use crate::middleware::PlatformAdmin;
use crate::password::hash_password;
use crate::state::AppState;

pub async fn list(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = sqlx::query(
        "SELECT id, username, display_name, is_platform_admin, is_super_admin, is_active, created_at, updated_at, is_ops
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
                "is_super_admin": r.get::<i64, _>(4) != 0,
                "is_active": r.get::<i64, _>(5) != 0,
                "created_at": r.get::<Option<String>, _>(6),
                "updated_at": r.get::<Option<String>, _>(7),
                "is_ops": r.get::<i64, _>(8) != 0,
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(users)))
}

pub async fn create(
    auth: PlatformAdmin,
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
    let is_super = req.is_super_admin.unwrap_or(false) && auth.is_super_admin;
    let is_ops = req.is_ops.unwrap_or(false);
    let display = req.display_name.unwrap_or_else(|| username.to_string());

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_ops, is_active)
         VALUES (?, ?, ?, ?, ?, ?, ?, 1)",
    )
    .bind(&id)
    .bind(username)
    .bind(&hash)
    .bind(&display)
    .bind(if is_admin { 1 } else { 0 })
    .bind(if is_super { 1 } else { 0 })
    .bind(if is_ops { 1 } else { 0 })
    .execute(&state.pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::Conflict("username already exists".into())
        } else {
            AppError::DatabaseError(e.to_string())
        }
    })?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::UserCreate)
            .resource("user", &id, username)
            .after(Some(serde_json::json!({
                "id": id,
                "username": username,
                "display_name": display,
                "is_platform_admin": is_admin,
                "is_super_admin": is_super,
                "is_ops": is_ops,
                "is_active": true,
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "username": username,
        "is_platform_admin": is_admin,
        "is_super_admin": is_super,
        "is_ops": is_ops,
    }))))
}

pub async fn update(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let before_row = sqlx::query(
        "SELECT id, username, display_name, is_platform_admin, is_super_admin, is_active FROM users WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(before_row) = before_row else {
        return Err(AppError::NotFound("user not found".into()));
    };
    let username: String = before_row.get(1);
    let before = serde_json::json!({
        "id": before_row.get::<String, _>(0),
        "username": username,
        "display_name": before_row.get::<String, _>(2),
        "is_platform_admin": before_row.get::<i64, _>(3) != 0,
        "is_super_admin": before_row.get::<i64, _>(4) != 0,
        "is_active": before_row.get::<i64, _>(5) != 0,
    });

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

    // Super admin tier may only be granted/revoked by another super admin.
    if let Some(is_super) = req.is_super_admin {
        if !auth.is_super_admin {
            return Err(AppError::Forbidden(
                "only super admins can change super admin status".into(),
            ));
        }
        if !is_super && id == auth.id {
            let super_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM users WHERE is_super_admin = 1 AND is_active = 1",
            )
            .fetch_one(&state.pool)
            .await?;
            if super_count <= 1 {
                return Err(AppError::ValidationFailed(
                    "cannot remove the last super admin".into(),
                ));
            }
        }
        sqlx::query(
            "UPDATE users SET is_super_admin = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(if is_super { 1 } else { 0 })
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }

    // Platform-ops flag: any platform admin may grant/revoke it.
    if let Some(is_ops) = req.is_ops {
        sqlx::query("UPDATE users SET is_ops = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(if is_ops { 1 } else { 0 })
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

    let after_row = sqlx::query(
        "SELECT id, username, display_name, is_platform_admin, is_super_admin, is_active, is_ops FROM users WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await?;
    let after = serde_json::json!({
        "id": after_row.get::<String, _>(0),
        "username": after_row.get::<String, _>(1),
        "display_name": after_row.get::<String, _>(2),
        "is_platform_admin": after_row.get::<i64, _>(3) != 0,
        "is_super_admin": after_row.get::<i64, _>(4) != 0,
        "is_active": after_row.get::<i64, _>(5) != 0,
        "is_ops": after_row.get::<i64, _>(6) != 0,
    });
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::UserUpdate)
            .resource("user", &id, &username)
            .before(Some(before))
            .after(Some(after))
            .meta(
                if req
                    .password
                    .as_ref()
                    .map(|p| !p.is_empty())
                    .unwrap_or(false)
                {
                    Some(serde_json::json!({ "password_changed": true }))
                } else {
                    None
                },
            ),
    )
    .await;

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
    let before_row = sqlx::query(
        "SELECT username, display_name, is_platform_admin, is_super_admin, is_active FROM users WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;
    let (username, before) = match before_row {
        Some(r) => {
            let username: String = r.get(0);
            (
                username.clone(),
                Some(serde_json::json!({
                    "id": id,
                    "username": username,
                    "display_name": r.get::<String, _>(1),
                    "is_platform_admin": r.get::<i64, _>(2) != 0,
                    "is_super_admin": r.get::<i64, _>(3) != 0,
                    "is_active": r.get::<i64, _>(4) != 0,
                })),
            )
        }
        None => (id.clone(), None),
    };
    let result = sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("user not found".into()));
    }
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::UserDelete)
            .resource("user", &id, &username)
            .before(before),
    )
    .await;
    Ok(Json(ApiResponse::success(())))
}
