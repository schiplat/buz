use axum::{Json, extract::{State, Path}};
use shared::protocol::{CreateTeamRequest, UpdateTeamRequest, UpsertTeamMemberRequest};
use shared::protocol::response::ApiResponse;
use shared::error::AppError;
use shared::rbac::{Perm, TeamRole};
use sqlx::Row;
use uuid::Uuid;

use crate::middleware::AuthUser;
use crate::state::AppState;

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = if auth.is_platform_admin {
        sqlx::query(
            "SELECT id, name, slug, description, created_at, updated_at FROM teams ORDER BY name ASC",
        )
        .fetch_all(&state.pool)
        .await?
    } else {
        let ids = auth.team_ids();
        if ids.is_empty() {
            return Ok(Json(ApiResponse::success(vec![])));
        }
        let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, name, slug, description, created_at, updated_at
             FROM teams WHERE id IN ({placeholders}) ORDER BY name ASC"
        );
        let mut q = sqlx::query(&sql);
        for id in &ids {
            q = q.bind(id);
        }
        q.fetch_all(&state.pool).await?
    };

    let teams = rows
        .into_iter()
        .map(|r| {
            let id: String = r.get(0);
            let my_role = auth.role_in(&id).map(|r| r.as_str());
            serde_json::json!({
                "id": id,
                "name": r.get::<String, _>(1),
                "slug": r.get::<String, _>(2),
                "description": r.get::<String, _>(3),
                "my_role": my_role,
                "created_at": r.get::<Option<String>, _>(4),
                "updated_at": r.get::<Option<String>, _>(5),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(teams)))
}

pub async fn create(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateTeamRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    auth.require_platform_admin()?;

    let name = req.name.trim();
    let slug = req.slug.trim().to_lowercase();
    if name.is_empty() || slug.is_empty() {
        return Err(AppError::ValidationFailed(
            "name and slug are required".into(),
        ));
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AppError::ValidationFailed(
            "slug must be alphanumeric with - or _".into(),
        ));
    }

    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO teams (id, name, slug, description) VALUES (?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(&slug)
    .bind(req.description.as_deref().unwrap_or(""))
    .execute(&state.pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::Conflict("team slug already exists".into())
        } else {
            AppError::DatabaseError(e.to_string())
        }
    })?;

    // Creator (platform admin) becomes owner if they also want membership — optional.
    // Platform admin already bypasses checks; still add as owner for visibility in /me.
    sqlx::query(
        "INSERT OR IGNORE INTO team_members (team_id, user_id, role) VALUES (?, ?, 'owner')",
    )
    .bind(&id)
    .bind(&auth.id)
    .execute(&state.pool)
    .await?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": name,
        "slug": slug,
    }))))
}

pub async fn get_one(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    auth.check_team(&id, Perm::TeamView)?;

    let r = sqlx::query(
        "SELECT id, name, slug, description, created_at, updated_at FROM teams WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("team not found".into()))?;

    let members = list_members_inner(&state, &id).await?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.get::<String, _>(0),
        "name": r.get::<String, _>(1),
        "slug": r.get::<String, _>(2),
        "description": r.get::<String, _>(3),
        "my_role": auth.role_in(&id).map(|r| r.as_str()),
        "created_at": r.get::<Option<String>, _>(4),
        "updated_at": r.get::<Option<String>, _>(5),
        "members": members,
    }))))
}

pub async fn update(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateTeamRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    auth.check_team(&id, Perm::TeamManageMembers)?;

    if let Some(ref name) = req.name {
        sqlx::query("UPDATE teams SET name = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(name.trim())
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }
    if let Some(ref desc) = req.description {
        sqlx::query(
            "UPDATE teams SET description = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(desc)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }

    Ok(Json(ApiResponse::success(serde_json::json!({ "id": id }))))
}

pub async fn delete(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    auth.check_team(&id, Perm::TeamDelete)?;

    let pipe_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pipelines WHERE team_id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await?;
    let secret_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM secrets WHERE team_id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await?;
    if pipe_count > 0 || secret_count > 0 {
        return Err(AppError::Conflict(format!(
            "team still has {pipe_count} pipeline(s) and {secret_count} secret(s)"
        )));
    }

    let result = sqlx::query("DELETE FROM teams WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("team not found".into()));
    }
    Ok(Json(ApiResponse::success(())))
}

pub async fn list_members(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    auth.check_team(&id, Perm::TeamView)?;
    let members = list_members_inner(&state, &id).await?;
    Ok(Json(ApiResponse::success(members)))
}

async fn list_members_inner(
    state: &AppState,
    team_id: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = sqlx::query(
        "SELECT u.id, u.username, u.display_name, tm.role
         FROM team_members tm
         JOIN users u ON u.id = tm.user_id
         WHERE tm.team_id = ?
         ORDER BY u.username ASC",
    )
    .bind(team_id)
    .fetch_all(&state.pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "user_id": r.get::<String, _>(0),
                "username": r.get::<String, _>(1),
                "display_name": r.get::<String, _>(2),
                "role": r.get::<String, _>(3),
            })
        })
        .collect())
}

pub async fn upsert_member(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpsertTeamMemberRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    auth.check_team(&id, Perm::TeamManageMembers)?;

    let Some(role) = TeamRole::parse(&req.role) else {
        return Err(AppError::ValidationFailed(
            "role must be owner|maintainer|developer|viewer".into(),
        ));
    };

    let user_exists: Option<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE id = ? AND is_active = 1")
            .bind(&req.user_id)
            .fetch_optional(&state.pool)
            .await?;
    if user_exists.is_none() {
        return Err(AppError::NotFound("user not found".into()));
    }

    sqlx::query(
        "INSERT INTO team_members (team_id, user_id, role) VALUES (?, ?, ?)
         ON CONFLICT(team_id, user_id) DO UPDATE SET role = excluded.role",
    )
    .bind(&id)
    .bind(&req.user_id)
    .bind(role.as_str())
    .execute(&state.pool)
    .await?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "team_id": id,
        "user_id": req.user_id,
        "role": role.as_str(),
    }))))
}

pub async fn remove_member(
    auth: AuthUser,
    Path((id, user_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    auth.check_team(&id, Perm::TeamManageMembers)?;

    // Keep at least one owner
    let current_role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM team_members WHERE team_id = ? AND user_id = ?",
    )
    .bind(&id)
    .bind(&user_id)
    .fetch_optional(&state.pool)
    .await?;

    if current_role.as_deref() == Some("owner") {
        let owner_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM team_members WHERE team_id = ? AND role = 'owner'",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await?;
        if owner_count <= 1 {
            return Err(AppError::ValidationFailed(
                "cannot remove the last team owner".into(),
            ));
        }
    }

    let result = sqlx::query("DELETE FROM team_members WHERE team_id = ? AND user_id = ?")
        .bind(&id)
        .bind(&user_id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("membership not found".into()));
    }
    Ok(Json(ApiResponse::success(())))
}
