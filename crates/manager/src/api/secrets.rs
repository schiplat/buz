use axum::{
    extract::{Path, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateSecretRequest, UpdateSecretRequest};
use shared::rbac::Perm;
use shared::secret_kind::{normalize_secret_kind, validate_secret_name};
use uuid::Uuid;

use crate::audit::{self, secret_snapshot, Action, AuditEvent};
use crate::crypto;
use crate::middleware::AuthUser;
use crate::rbac::{resolve_required_team_id, secret_team_id, team_in_clause};
use crate::state::AppState;

/// Decrypt for API display with partial redaction. Plaintext/corrupt rows are
/// not shown — operators must re-save (`reencrypt_required: true`).
fn value_fields_for_api(stored: &str, jwt_secret: &str) -> (String, bool) {
    match crypto::decrypt(stored, jwt_secret) {
        Ok(plain) => (crypto::mask_for_display(&plain), false),
        Err(_) => ("****".to_string(), true),
    }
}

fn team_shared_from_row(v: i64) -> bool {
    v != 0
}

/// env_name no longer exists: the secret `name` IS the injected env key and
/// the `${secrets.NAME}` template reference, so it must be UPPER_SNAKE_CASE.
/// `name` is globally UNIQUE (create-time check below).
pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let team_filter = team_in_clause(&auth, "team_id");
    let rows = crate::store::secrets::list(
        &state.pool,
        team_filter.as_ref().map(|(_, ids)| ids.as_slice()),
    )
    .await?;
    let jwt = &state.config.auth.jwt_secret;

    // Batch-load GitHub bind summaries so list rows can render bound repos.
    let secret_ids: Vec<String> = rows.iter().map(|s| s.id.clone()).collect();
    let binds_map = super::github_sync::secret_binds_map(&state.pool, &secret_ids).await?;

    let secrets: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|s| {
            let (value, reencrypt_required) = value_fields_for_api(&s.value, jwt);
            let github_binds = binds_map.get(&s.id).cloned().unwrap_or_default();
            serde_json::json!({
                "id": s.id,
                "name": s.name,
                "value": value,
                "reencrypt_required": reencrypt_required,
                "owner_group": s.owner_group,
                "team_id": s.team_id,
                "team_shared": team_shared_from_row(s.team_shared),
                "kind": normalize_secret_kind(Some(&s.kind)),
                "updated_at": s.updated_at,
                "github_binds": github_binds,
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
    validate_secret_name(&name).map_err(AppError::ValidationFailed)?;
    if req.value.is_empty() {
        return Err(AppError::ValidationFailed("value is required".into()));
    }

    let (team_id, slug) = resolve_required_team_id(
        &state.pool,
        req.team_id.as_deref(),
        req.owner_group.as_deref(),
    )
    .await?;
    auth.check_team(&team_id, Perm::SecretManage)?;

    if crate::store::secrets::find_id_by_name(&state.pool, &name)
        .await?
        .is_some()
    {
        return Err(AppError::Conflict(format!(
            "secret '{name}' already exists"
        )));
    }

    let kind = normalize_secret_kind(req.kind.as_deref());

    let id = Uuid::new_v4().to_string();

    let encrypted = crypto::encrypt(&req.value, &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("secret encryption failed: {e}")))?;

    let team_shared = req.team_shared.unwrap_or(false);
    let team_shared_i: i64 = if team_shared { 1 } else { 0 };

    crate::store::secrets::insert(
        &state.pool,
        &id,
        &name,
        &encrypted,
        &slug,
        &team_id,
        team_shared_i,
        kind,
    )
    .await?;

    tracing::info!(
        "Secret created: {name} ({id}) team={slug} kind={kind} team_shared={team_shared}"
    );

    let after = secret_snapshot(&id, &name, &slug, &team_id, team_shared, kind);
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::SecretCreate)
            .resource("secret", &id, &name)
            .team(Some(team_id.clone()))
            .after(Some(after)),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": name,
        "owner_group": slug,
        "team_id": team_id,
        "team_shared": team_shared,
        "kind": kind,
    }))))
}

pub async fn get_one(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = secret_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::TeamView)?;

    let s = crate::store::secrets::find_by_id(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("secret not found".into()))?;

    let (value, reencrypt_required) = value_fields_for_api(&s.value, &state.config.auth.jwt_secret);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": s.id,
        "name": s.name,
        "value": value,
        "reencrypt_required": reencrypt_required,
        "owner_group": s.owner_group,
        "team_id": s.team_id,
        "team_shared": team_shared_from_row(s.team_shared),
        "kind": normalize_secret_kind(Some(&s.kind)),
        "updated_at": s.updated_at,
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
        let slug = crate::store::secrets::owner_group_of(&state.pool, &id).await?;
        (team_id, slug)
    };

    let facts = crate::store::secrets::update_facts(&state.pool, &id).await?;
    let before_name = facts.name;
    let before_slug = facts.owner_group;
    let before_team = facts.team_id;
    let current_shared = facts.team_shared;
    let current_kind = facts.kind;
    let current_value = facts.value;
    let before = secret_snapshot(
        &id,
        &before_name,
        &before_slug,
        &before_team,
        current_shared != 0,
        normalize_secret_kind(Some(&current_kind)),
    );

    let team_shared_i = req
        .team_shared
        .map(|v| if v { 1 } else { 0 })
        .unwrap_or(current_shared);
    let kind = req
        .kind
        .as_deref()
        .map(|k| normalize_secret_kind(Some(k)).to_string())
        .unwrap_or_else(|| normalize_secret_kind(Some(&current_kind)).to_string());

    if req.value.is_empty() {
        crate::store::secrets::update_metadata(
            &state.pool,
            &id,
            &slug,
            &new_team_id,
            team_shared_i,
            &kind,
        )
        .await?;
    } else {
        let encrypted = crypto::encrypt(&req.value, &state.config.auth.jwt_secret)
            .map_err(|e| AppError::Internal(format!("secret encryption failed: {e}")))?;
        crate::store::secrets::update_value_and_metadata(
            &state.pool,
            &id,
            &encrypted,
            &slug,
            &new_team_id,
            team_shared_i,
            &kind,
        )
        .await?;
    }

    // Dirty-mark inherit GitHub binds when the platform default value actually changed.
    // Custom-override binds are intentionally left alone (their effective push value
    // does not depend on the platform default).
    let mut github_binds_stale: u64 = 0;
    if !req.value.is_empty() {
        let value_changed = match crypto::decrypt(&current_value, &state.config.auth.jwt_secret) {
            Ok(plain) => plain != req.value,
            Err(_) => true, // corrupt / legacy plaintext — treat as a real change
        };
        if value_changed {
            github_binds_stale =
                super::github_sync::mark_secret_binds_stale_on_default_change(&state.pool, &id)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            secret_id = %id,
                            error = %e,
                            "github-sync: failed to mark secret binds stale"
                        );
                        0
                    });
        }
    }

    let after = secret_snapshot(
        &id,
        &before_name,
        &slug,
        &new_team_id,
        team_shared_i != 0,
        &kind,
    );
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::SecretUpdate)
            .resource("secret", &id, &before_name)
            .team(Some(new_team_id.clone()))
            .before(Some(before))
            .after(Some(after)),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "updated": true,
        "team_id": new_team_id,
        "team_shared": team_shared_i != 0,
        "kind": kind,
        "github_binds_stale": github_binds_stale,
    }))))
}

pub async fn delete(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let team_id = secret_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::SecretManage)?;

    let facts = crate::store::secrets::delete_facts(&state.pool, &id).await?;
    let name = facts.name;
    let slug = facts.owner_group;
    let tid = facts.team_id;
    let shared = facts.team_shared;
    let kind = facts.kind;
    let before = secret_snapshot(
        &id,
        &name,
        &slug,
        &tid,
        shared != 0,
        normalize_secret_kind(Some(&kind)),
    );

    crate::store::secrets::delete(&state.pool, &id).await?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::SecretDelete)
            .resource("secret", &id, &name)
            .team(Some(tid))
            .before(Some(before)),
    )
    .await;

    tracing::info!("Secret deleted: {name} ({id})");
    Ok(Json(ApiResponse::success(())))
}
