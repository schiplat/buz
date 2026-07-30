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

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let team_filter = team_in_clause(&auth, "team_id");
    let sql = match &team_filter {
        None => {
            "SELECT id, name, value, owner_group, team_id, team_shared, updated_at FROM secrets ORDER BY name ASC"
                .to_string()
        }
        Some((clause, _)) if clause == "AND 1=0" => {
            return Ok(Json(ApiResponse::success(vec![])));
        }
        Some((clause, _)) => format!(
            "SELECT id, name, value, owner_group, team_id, team_shared, updated_at FROM secrets WHERE 1=1 {clause} ORDER BY name ASC"
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
    let secrets: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let stored: String = r.get(2);
            let (value, reencrypt_required) = value_fields_for_api(&stored, jwt);
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "name": r.get::<String, _>(1),
                "value": value,
                "reencrypt_required": reencrypt_required,
                "owner_group": r.get::<String, _>(3),
                "team_id": r.get::<String, _>(4),
                "team_shared": team_shared_from_row(r.get::<i64, _>(5)),
                "updated_at": r.get::<Option<String>, _>(6),
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

    let encrypted = crypto::encrypt(&req.value, &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("secret encryption failed: {e}")))?;

    let team_shared = req.team_shared.unwrap_or(false);
    let team_shared_i: i64 = if team_shared { 1 } else { 0 };

    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO secrets (id, name, value, owner_group, team_id, team_shared) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&encrypted)
    .bind(&slug)
    .bind(&team_id)
    .bind(team_shared_i)
    .execute(&state.pool)
    .await?;

    tracing::info!(
        "Secret created: {name} ({id}) team={slug} team_shared={team_shared}"
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": name,
        "owner_group": slug,
        "team_id": team_id,
        "team_shared": team_shared,
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
        "SELECT id, name, value, owner_group, team_id, team_shared, updated_at FROM secrets WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("secret not found".into()))?;

    let stored: String = r.get(2);
    let (value, reencrypt_required) =
        value_fields_for_api(&stored, &state.config.auth.jwt_secret);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.get::<String, _>(0),
        "name": r.get::<String, _>(1),
        "value": value,
        "reencrypt_required": reencrypt_required,
        "owner_group": r.get::<String, _>(3),
        "team_id": r.get::<String, _>(4),
        "team_shared": team_shared_from_row(r.get::<i64, _>(5)),
        "updated_at": r.get::<Option<String>, _>(6),
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

    let team_shared_i: Option<i64> = req.team_shared.map(|v| if v { 1 } else { 0 });

    // Empty value = keep existing ciphertext (UI: leave blank to keep current).
    if req.value.is_empty() {
        if let Some(shared) = team_shared_i {
            sqlx::query(
                "UPDATE secrets SET owner_group = ?, team_id = ?, team_shared = ?, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(&slug)
            .bind(&new_team_id)
            .bind(shared)
            .bind(&id)
            .execute(&state.pool)
            .await?;
        } else {
            sqlx::query(
                "UPDATE secrets SET owner_group = ?, team_id = ?, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(&slug)
            .bind(&new_team_id)
            .bind(&id)
            .execute(&state.pool)
            .await?;
        }
    } else {
        let encrypted = crypto::encrypt(&req.value, &state.config.auth.jwt_secret)
            .map_err(|e| AppError::Internal(format!("secret encryption failed: {e}")))?;
        if let Some(shared) = team_shared_i {
            sqlx::query(
                "UPDATE secrets SET value = ?, owner_group = ?, team_id = ?, team_shared = ?, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(&encrypted)
            .bind(&slug)
            .bind(&new_team_id)
            .bind(shared)
            .bind(&id)
            .execute(&state.pool)
            .await?;
        } else {
            sqlx::query(
                "UPDATE secrets SET value = ?, owner_group = ?, team_id = ?, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(&encrypted)
            .bind(&slug)
            .bind(&new_team_id)
            .bind(&id)
            .execute(&state.pool)
            .await?;
        }
    }

    let team_shared: bool = sqlx::query_scalar("SELECT team_shared FROM secrets WHERE id = ?")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .map(|v: i64| v != 0)
        .unwrap_or(false);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "updated": true,
        "team_id": new_team_id,
        "team_shared": team_shared,
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
