use axum::{
    extract::{Path, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateTeamRequest, UpdateTeamRequest, UpsertTeamMemberRequest};
use shared::rbac::{Perm, TeamRole};
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent};
use crate::middleware::AuthUser;
use crate::state::AppState;

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let visible = if auth.is_platform_admin {
        None
    } else {
        Some(auth.team_ids())
    };
    let rows = crate::store::teams::list(&state.pool, visible.as_deref()).await?;

    let teams = rows
        .into_iter()
        .map(|t| {
            let my_role = auth.role_in(&t.id).map(|r| r.as_str());
            serde_json::json!({
                "id": t.id,
                "name": t.name,
                "slug": t.slug,
                "description": t.description,
                "my_role": my_role,
                "created_at": t.created_at,
                "updated_at": t.updated_at,
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
    crate::store::teams::insert(
        &state.pool,
        &id,
        name,
        &slug,
        req.description.as_deref().unwrap_or(""),
    )
    .await?;

    // Creator (platform admin) becomes owner if they also want membership — optional.
    // Platform admin already bypasses checks; still add as owner for visibility in /me.
    crate::store::teams::attach_owner(&state.pool, &id, &auth.id).await?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::TeamCreate)
            .resource("team", &id, name)
            .team(Some(id.clone()))
            .after(Some(serde_json::json!({
                "id": id,
                "name": name,
                "slug": slug,
                "description": req.description.as_deref().unwrap_or(""),
            }))),
    )
    .await;

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

    let t = crate::store::teams::find_by_id(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("team not found".into()))?;

    let members = list_members_inner(&state, &id).await?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": t.id,
        "name": t.name,
        "slug": t.slug,
        "description": t.description,
        "my_role": auth.role_in(&id).map(|r| r.as_str()),
        "created_at": t.created_at,
        "updated_at": t.updated_at,
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

    let before_facts = crate::store::teams::facts(&state.pool, &id).await?;
    let before = before_facts.as_ref().map(|f| {
        serde_json::json!({
            "id": id,
            "name": f.name,
            "slug": f.slug,
            "description": f.description,
        })
    });
    let team_name = before_facts
        .as_ref()
        .map(|f| f.name.clone())
        .unwrap_or_else(|| id.clone());

    if let Some(ref name) = req.name {
        crate::store::teams::update_name(&state.pool, &id, name.trim()).await?;
    }
    if let Some(ref desc) = req.description {
        crate::store::teams::update_description(&state.pool, &id, desc).await?;
    }

    let after_facts = crate::store::teams::facts(&state.pool, &id).await?;
    let after = after_facts.as_ref().map(|f| {
        serde_json::json!({
            "id": id,
            "name": f.name,
            "slug": f.slug,
            "description": f.description,
        })
    });
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::TeamUpdate)
            .resource("team", &id, &team_name)
            .team(Some(id.clone()))
            .before(before)
            .after(after),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({ "id": id }))))
}

pub async fn delete(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    auth.check_team(&id, Perm::TeamDelete)?;

    let pipe_count = crate::store::teams::pipeline_count(&state.pool, &id).await?;
    let secret_count = crate::store::teams::secret_count(&state.pool, &id).await?;
    let runner_count = crate::store::teams::runner_count(&state.pool, &id).await?;
    if pipe_count > 0 || secret_count > 0 || runner_count > 0 {
        return Err(AppError::Conflict(format!(
            "team still has {pipe_count} pipeline(s), {secret_count} secret(s), and {runner_count} bound runner(s)"
        )));
    }

    let facts = crate::store::teams::facts(&state.pool, &id).await?;
    let (team_name, before) = match facts {
        Some(f) => {
            let name = f.name.clone();
            (
                name.clone(),
                Some(serde_json::json!({
                    "id": id,
                    "name": name,
                    "slug": f.slug,
                    "description": f.description,
                })),
            )
        }
        None => (id.clone(), None),
    };

    let deleted = crate::store::teams::delete(&state.pool, &id).await?;
    if !deleted {
        return Err(AppError::NotFound("team not found".into()));
    }
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::TeamDelete)
            .resource("team", &id, &team_name)
            .team(Some(id.clone()))
            .before(before),
    )
    .await;
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
    crate::store::teams::list_members(&state.pool, team_id).await
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

    let user_exists = crate::store::teams::active_user_id(&state.pool, &req.user_id).await?;
    if user_exists.is_none() {
        return Err(AppError::NotFound("user not found".into()));
    }

    let prev = crate::store::teams::member_role(&state.pool, &id, &req.user_id).await?;

    crate::store::teams::upsert_member(&state.pool, &id, &req.user_id, role.as_str()).await?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::TeamMemberUpsert)
            .resource("team_member", &req.user_id, &req.user_id)
            .team(Some(id.clone()))
            .before(prev.map(|r| serde_json::json!({ "role": r })))
            .after(Some(serde_json::json!({
                "team_id": id,
                "user_id": req.user_id,
                "role": role.as_str(),
            }))),
    )
    .await;

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
    let current_role = crate::store::teams::member_role(&state.pool, &id, &user_id).await?;

    if current_role.as_deref() == Some("owner") {
        let owner_count = crate::store::teams::owner_count(&state.pool, &id).await?;
        if owner_count <= 1 {
            return Err(AppError::ValidationFailed(
                "cannot remove the last team owner".into(),
            ));
        }
    }

    let removed = crate::store::teams::remove_member(&state.pool, &id, &user_id).await?;
    if !removed {
        return Err(AppError::NotFound("membership not found".into()));
    }
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::TeamMemberRemove)
            .resource("team_member", &user_id, &user_id)
            .team(Some(id.clone()))
            .before(current_role.map(|r| serde_json::json!({ "role": r, "user_id": user_id }))),
    )
    .await;
    Ok(Json(ApiResponse::success(())))
}
