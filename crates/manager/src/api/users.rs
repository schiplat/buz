use axum::{
    extract::{Path, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateUserRequest, UpdateUserRequest};
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent};
use crate::middleware::PlatformAdmin;
use crate::password::hash_password;
use crate::state::AppState;

pub async fn list(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = crate::store::users::list_all(&state.pool).await?;

    let users = rows
        .into_iter()
        .map(|u| {
            serde_json::json!({
                "id": u.id,
                "username": u.username,
                "display_name": u.display_name,
                "is_platform_admin": u.is_platform_admin,
                "is_super_admin": u.is_super_admin,
                "is_active": u.is_active,
                "created_at": u.created_at,
                "updated_at": u.updated_at,
                "is_ops": u.is_ops,
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

    crate::store::users::insert(
        &state.pool,
        &id,
        username,
        &hash,
        &display,
        is_admin,
        is_super,
        is_ops,
    )
    .await?;

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
    let facts = crate::store::users::update_facts(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;
    let username = facts.username.clone();
    let before = serde_json::json!({
        "id": facts.id,
        "username": facts.username,
        "display_name": facts.display_name,
        "is_platform_admin": facts.is_platform_admin,
        "is_super_admin": facts.is_super_admin,
        "is_active": facts.is_active,
    });

    if let Some(ref pw) = req.password {
        if pw.is_empty() {
            return Err(AppError::ValidationFailed(
                "password cannot be empty".into(),
            ));
        }
        let hash = hash_password(pw).map_err(AppError::Internal)?;
        crate::store::users::update_password(&state.pool, &id, &hash).await?;
    }

    if let Some(ref dn) = req.display_name {
        crate::store::users::update_display_name(&state.pool, &id, dn).await?;
    }

    if let Some(is_admin) = req.is_platform_admin {
        // Prevent removing the last platform admin
        if !is_admin && id == auth.id {
            let admin_count =
                crate::store::users::count_active_platform_admins(&state.pool).await?;
            if admin_count <= 1 {
                return Err(AppError::ValidationFailed(
                    "cannot remove the last platform admin".into(),
                ));
            }
        }
        crate::store::users::update_is_platform_admin(&state.pool, &id, is_admin).await?;
    }

    // Super admin tier may only be granted/revoked by another super admin.
    if let Some(is_super) = req.is_super_admin {
        if !auth.is_super_admin {
            return Err(AppError::Forbidden(
                "only super admins can change super admin status".into(),
            ));
        }
        if !is_super && id == auth.id {
            let super_count = crate::store::users::count_active_super_admins(&state.pool).await?;
            if super_count <= 1 {
                return Err(AppError::ValidationFailed(
                    "cannot remove the last super admin".into(),
                ));
            }
        }
        crate::store::users::update_is_super_admin(&state.pool, &id, is_super).await?;
    }

    // Platform-ops flag: any platform admin may grant/revoke it.
    if let Some(is_ops) = req.is_ops {
        crate::store::users::update_is_ops(&state.pool, &id, is_ops).await?;
    }

    if let Some(active) = req.is_active {
        if !active && id == auth.id {
            return Err(AppError::ValidationFailed(
                "cannot deactivate yourself".into(),
            ));
        }
        crate::store::users::update_is_active(&state.pool, &id, active).await?;
    }

    let after_facts = crate::store::users::update_after(&state.pool, &id).await?;
    let after = serde_json::json!({
        "id": after_facts.id,
        "username": after_facts.username,
        "display_name": after_facts.display_name,
        "is_platform_admin": after_facts.is_platform_admin,
        "is_super_admin": after_facts.is_super_admin,
        "is_active": after_facts.is_active,
        "is_ops": after_facts.is_ops,
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
    let facts = crate::store::users::delete_facts(&state.pool, &id).await?;
    let (username, before) = match facts {
        Some(f) => {
            let username = f.username.clone();
            (
                username.clone(),
                Some(serde_json::json!({
                    "id": id,
                    "username": username,
                    "display_name": f.display_name,
                    "is_platform_admin": f.is_platform_admin,
                    "is_super_admin": f.is_super_admin,
                    "is_active": f.is_active,
                })),
            )
        }
        None => (id.clone(), None),
    };
    let deleted = crate::store::users::delete(&state.pool, &id).await?;
    if !deleted {
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
