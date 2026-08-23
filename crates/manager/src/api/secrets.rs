use axum::{
    extract::{Path, State},
    Json,
};
use shared::config_auth::parse_oss_secret_value;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateSecretRequest, UpdateSecretRequest};
use shared::rbac::Perm;
use shared::registry_auth::{looks_like_registry_secret_value, parse_registry_secret_value};
use shared::secret_kind::{
    is_aliyun_oss_auth_kind, is_registry_kind, normalize_secret_kind, validate_secret_name,
    SECRET_KIND_REGISTRY,
};
use sqlx::Row;
use uuid::Uuid;

use crate::audit::{self, secret_snapshot, AuditEvent};
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

/// Registry kind must contain parseable registry credential JSON.
fn validate_registry_value(value: &str) -> Result<(), AppError> {
    parse_registry_secret_value(value, Some("registry.example.com/ns/img:tag")).map_err(|e| {
        AppError::ValidationFailed(format!(
            "kind=registry requires JSON with username/password \
             (optional registry, provider): {e}"
        ))
    })?;
    Ok(())
}

/// OSS kind must contain parseable OSS read-only credential JSON.
fn validate_oss_value(value: &str) -> Result<(), AppError> {
    parse_oss_secret_value(value).map_err(|e| {
        AppError::ValidationFailed(format!(
            "kind=aliyun_oss_auth requires JSON with endpoint/bucket/access_key_id/access_key_secret \
             (optional region, provider): {e}"
        ))
    })?;
    Ok(())
}

/// env_name no longer exists: the secret `name` IS the injected env key and
/// the `${secrets.NAME}` template reference, so it must be UPPER_SNAKE_CASE.
/// `name` is globally UNIQUE (create-time check below).
pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let team_filter = team_in_clause(&auth, "team_id");
    let sql = match &team_filter {
        None => {
            "SELECT id, name, value, owner_group, team_id, team_shared, kind, updated_at FROM secrets ORDER BY name ASC"
                .to_string()
        }
        Some((clause, _)) if clause == "AND 1=0" => {
            return Ok(Json(ApiResponse::success(vec![])));
        }
        Some((clause, _)) => format!(
            "SELECT id, name, value, owner_group, team_id, team_shared, kind, updated_at FROM secrets WHERE 1=1 {clause} ORDER BY name ASC"
        ),
    };

    let mut q = sqlx::query(&sql);
    if let Some((_, ids)) = &team_filter {
        for id in ids {
            q = q.bind(id);
        }
    }

    let rows = q.fetch_all(&state.pool).await?;
    let jwt = &state.config.auth.jwt_secret;

    // Batch-load GitHub bind summaries so list rows can render bound repos.
    let secret_ids: Vec<String> = rows.iter().map(|r| r.get::<String, _>(0)).collect();
    let binds_map = super::github_sync::secret_binds_map(&state.pool, &secret_ids).await?;

    let secrets: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let stored: String = r.get(2);
            let (value, reencrypt_required) = value_fields_for_api(&stored, jwt);
            let kind_raw: String = r.get(6);
            let id: String = r.get(0);
            let name: String = r.get(1);
            serde_json::json!({
                "id": id,
                "name": name,
                "value": value,
                "reencrypt_required": reencrypt_required,
                "owner_group": r.get::<String, _>(3),
                "team_id": r.get::<String, _>(4),
                "team_shared": team_shared_from_row(r.get::<i64, _>(5)),
                "kind": normalize_secret_kind(Some(&kind_raw)),
                "updated_at": r.get::<Option<String>, _>(7),
                "github_binds": binds_map.get(&id).cloned().unwrap_or_default(),
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

    let existing = sqlx::query("SELECT id FROM secrets WHERE name = ?")
        .bind(&name)
        .fetch_optional(&state.pool)
        .await?;
    if existing.is_some() {
        return Err(AppError::Conflict(format!(
            "secret '{name}' already exists"
        )));
    }

    let kind = normalize_secret_kind(req.kind.as_deref());
    if is_registry_kind(kind) {
        validate_registry_value(&req.value)?;
    } else if is_aliyun_oss_auth_kind(kind) {
        validate_oss_value(&req.value)?;
    }

    let id = Uuid::new_v4().to_string();

    let encrypted = crypto::encrypt(&req.value, &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("secret encryption failed: {e}")))?;

    let team_shared = req.team_shared.unwrap_or(false);
    let team_shared_i: i64 = if team_shared { 1 } else { 0 };

    sqlx::query(
        "INSERT INTO secrets (id, name, value, owner_group, team_id, team_shared, kind) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&encrypted)
    .bind(&slug)
    .bind(&team_id)
    .bind(team_shared_i)
    .bind(kind)
    .execute(&state.pool)
    .await?;

    tracing::info!(
        "Secret created: {name} ({id}) team={slug} kind={kind} team_shared={team_shared}"
    );

    let after = secret_snapshot(&id, &name, &slug, &team_id, team_shared, kind);
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, "secret.create")
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

    let r = sqlx::query(
        "SELECT id, name, value, owner_group, team_id, team_shared, kind, updated_at FROM secrets WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("secret not found".into()))?;

    let stored: String = r.get(2);
    let (value, reencrypt_required) = value_fields_for_api(&stored, &state.config.auth.jwt_secret);
    let kind_raw: String = r.get(6);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.get::<String, _>(0),
        "name": r.get::<String, _>(1),
        "value": value,
        "reencrypt_required": reencrypt_required,
        "owner_group": r.get::<String, _>(3),
        "team_id": r.get::<String, _>(4),
        "team_shared": team_shared_from_row(r.get::<i64, _>(5)),
        "kind": normalize_secret_kind(Some(&kind_raw)),
        "updated_at": r.get::<Option<String>, _>(7),
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

    let before_row = sqlx::query(
        "SELECT name, owner_group, team_id, team_shared, kind, value FROM secrets WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await?;
    let before_name: String = before_row.get(0);
    let before_slug: String = before_row.get(1);
    let before_team: String = before_row.get(2);
    let current_shared: i64 = before_row.get(3);
    let current_kind: String = before_row.get(4);
    let current_value: String = before_row.get(5);
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
        if is_registry_kind(&kind) {
            match crypto::decrypt(&current_value, &state.config.auth.jwt_secret) {
                Ok(plain) => validate_registry_value(&plain)?,
                Err(_) => {
                    return Err(AppError::ValidationFailed(
                        "kind=registry requires a valid registry JSON value — re-enter Value"
                            .into(),
                    ));
                }
            }
        } else if is_aliyun_oss_auth_kind(&kind) {
            match crypto::decrypt(&current_value, &state.config.auth.jwt_secret) {
                Ok(plain) => validate_oss_value(&plain)?,
                Err(_) => {
                    return Err(AppError::ValidationFailed(
                        "kind=aliyun_oss_auth requires a valid OSS JSON value — re-enter Value"
                            .into(),
                    ));
                }
            }
        }
        sqlx::query(
            "UPDATE secrets SET owner_group = ?, team_id = ?, team_shared = ?, kind = ?, \
             updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&slug)
        .bind(&new_team_id)
        .bind(team_shared_i)
        .bind(&kind)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    } else {
        if is_registry_kind(&kind) {
            validate_registry_value(&req.value)?;
        } else if is_aliyun_oss_auth_kind(&kind) {
            validate_oss_value(&req.value)?;
        } else if looks_like_registry_secret_value(&req.value) && kind != SECRET_KIND_REGISTRY {
            // Allow generic secrets that happen to be JSON; no hard fail.
        }
        let encrypted = crypto::encrypt(&req.value, &state.config.auth.jwt_secret)
            .map_err(|e| AppError::Internal(format!("secret encryption failed: {e}")))?;
        sqlx::query(
            "UPDATE secrets SET value = ?, owner_group = ?, team_id = ?, team_shared = ?, kind = ?, \
             updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&encrypted)
        .bind(&slug)
        .bind(&new_team_id)
        .bind(team_shared_i)
        .bind(&kind)
        .bind(&id)
        .execute(&state.pool)
        .await?;
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
        AuditEvent::from_auth(&auth, "secret.update")
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
    }))))
}

pub async fn delete(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let team_id = secret_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::SecretManage)?;

    let row = sqlx::query(
        "SELECT name, owner_group, team_id, team_shared, kind FROM secrets WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await?;
    let name: String = row.get(0);
    let slug: String = row.get(1);
    let tid: String = row.get(2);
    let shared: i64 = row.get(3);
    let kind: String = row.get(4);
    let before = secret_snapshot(
        &id,
        &name,
        &slug,
        &tid,
        shared != 0,
        normalize_secret_kind(Some(&kind)),
    );

    sqlx::query("DELETE FROM secrets WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, "secret.delete")
            .resource("secret", &id, &name)
            .team(Some(tid))
            .before(Some(before)),
    )
    .await;

    tracing::info!("Secret deleted: {name} ({id})");
    Ok(Json(ApiResponse::success(())))
}
