use axum::{
    extract::{Path, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateHostRequest, TestHostRequest, TestHostResponse, UpdateHostRequest};
use shared::rbac::Perm;
use sqlx::Row;
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent};
use crate::crypto;
use crate::middleware::AuthUser;
use crate::rbac::{host_team_id, resolve_optional_team_id};
use crate::ssh_probe::{self, ProbeParams};
use crate::state::AppState;

fn host_json(r: &sqlx::sqlite::SqliteRow) -> serde_json::Value {
    let labels_raw: String = r.get(7);
    let cred_data: String = r.get(6);
    let team_id: Option<String> = r.try_get(10).ok().flatten();
    serde_json::json!({
        "id": r.get::<String, _>(0),
        "name": r.get::<String, _>(1),
        "host": r.get::<String, _>(2),
        "port": r.get::<i64, _>(3) as u16,
        "username": r.get::<String, _>(4),
        "credential_type": r.get::<String, _>(5),
        "credential_stored": !cred_data.is_empty(),
        "labels": serde_json::from_str::<Vec<String>>(&labels_raw).unwrap_or_default(),
        "created_at": r.get::<Option<String>, _>(8),
        "updated_at": r.get::<Option<String>, _>(9),
        "team_id": team_id,
    })
}

/// Shared hosts (NULL team_id) are visible to all authenticated users;
/// team-scoped hosts only to members (or platform admin).
async fn assert_host_visible(auth: &AuthUser, team_id: &Option<String>) -> Result<(), AppError> {
    match team_id {
        None => Ok(()),
        Some(tid) if tid.is_empty() => Ok(()),
        Some(tid) => auth.check_team(tid, Perm::TeamView),
    }
}

async fn assert_host_manage(auth: &AuthUser, team_id: &Option<String>) -> Result<(), AppError> {
    match team_id {
        None => auth.require_platform_admin(),
        Some(tid) if tid.is_empty() => auth.require_platform_admin(),
        Some(tid) => auth.check_team(tid, Perm::HostManage),
    }
}

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = if auth.is_platform_admin {
        sqlx::query(
            "SELECT id, name, host, port, username, credential_type, credential_data, labels,
                    created_at, updated_at, team_id
             FROM hosts ORDER BY created_at DESC",
        )
        .fetch_all(&state.pool)
        .await?
    } else {
        let ids = auth.team_ids();
        if ids.is_empty() {
            sqlx::query(
                "SELECT id, name, host, port, username, credential_type, credential_data, labels,
                        created_at, updated_at, team_id
                 FROM hosts WHERE team_id IS NULL OR team_id = ''
                 ORDER BY created_at DESC",
            )
            .fetch_all(&state.pool)
            .await?
        } else {
            let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT id, name, host, port, username, credential_type, credential_data, labels,
                        created_at, updated_at, team_id
                 FROM hosts
                 WHERE team_id IS NULL OR team_id = '' OR team_id IN ({placeholders})
                 ORDER BY created_at DESC"
            );
            let mut q = sqlx::query(&sql);
            for id in &ids {
                q = q.bind(id);
            }
            q.fetch_all(&state.pool).await?
        }
    };

    let hosts: Vec<serde_json::Value> = rows.iter().map(host_json).collect();
    Ok(Json(ApiResponse::success(hosts)))
}

pub async fn create(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateHostRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team = resolve_optional_team_id(&state.pool, req.team_id.as_deref()).await?;
    let team_id_opt = team.as_ref().map(|(id, _)| id.clone());
    assert_host_manage(&auth, &team_id_opt).await?;

    let id = Uuid::new_v4().to_string();
    let cred_type = req.credential_type.unwrap_or_else(|| "vault".into());
    let labels_json =
        serde_json::to_string(&req.labels.unwrap_or_default()).unwrap_or_else(|_| "[]".into());
    let credential_data = req.credential_data.unwrap_or_default();
    let encrypted_cred_data = if credential_data.is_empty() {
        String::new()
    } else {
        crypto::encrypt(&credential_data, &state.config.auth.jwt_secret)
            .map_err(|e| AppError::Internal(format!("credential encryption failed: {e}")))?
    };

    let passphrase = req.credential_passphrase.unwrap_or_default();
    let encrypted_passphrase = if passphrase.is_empty() {
        String::new()
    } else {
        crypto::encrypt(&passphrase, &state.config.auth.jwt_secret)
            .map_err(|e| AppError::Internal(format!("passphrase encryption failed: {e}")))?
    };

    sqlx::query(
        "INSERT INTO hosts (id, name, host, port, username, credential_type, credential_data,
         credential_passphrase, labels, team_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&req.name)
    .bind(&req.host)
    .bind(req.port as i64)
    .bind(&req.username)
    .bind(&cred_type)
    .bind(&encrypted_cred_data)
    .bind(&encrypted_passphrase)
    .bind(&labels_json)
    .bind(&team_id_opt)
    .execute(&state.pool)
    .await?;

    tracing::info!("Host created: {} ({id})", req.name);

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::HostCreate)
            .resource("host", &id, &req.name)
            .team(team_id_opt.clone())
            .after(Some(serde_json::json!({
                "id": id,
                "name": req.name,
                "host": req.host,
                "port": req.port,
                "username": req.username,
                "credential_type": cred_type,
                "team_id": team_id_opt,
                "credential_data": if credential_data.is_empty() { "" } else { "***" },
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": req.name,
        "team_id": team_id_opt,
    }))))
}

pub async fn get_one(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = host_team_id(&state.pool, &id).await?;
    assert_host_visible(&auth, &team_id).await?;

    let r = sqlx::query(
        "SELECT id, name, host, port, username, credential_type, credential_data, labels,
                created_at, updated_at, team_id
         FROM hosts WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("host not found".into()))?;

    Ok(Json(ApiResponse::success(host_json(&r))))
}

pub async fn update(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateHostRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let current_team = host_team_id(&state.pool, &id).await?;
    assert_host_manage(&auth, &current_team).await?;

    let before_row = sqlx::query(
        "SELECT name, host, port, username, credential_type, team_id FROM hosts WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;
    let before = before_row.as_ref().map(|r| {
        serde_json::json!({
            "id": id,
            "name": r.get::<String, _>(0),
            "host": r.get::<String, _>(1),
            "port": r.get::<i64, _>(2),
            "username": r.get::<String, _>(3),
            "credential_type": r.get::<String, _>(4),
            "team_id": r.get::<Option<String>, _>(5),
        })
    });
    let before_name = before_row
        .as_ref()
        .map(|r| r.get::<String, _>(0))
        .unwrap_or_default();

    if let Some(ref tid) = req.team_id {
        let new_team = if tid.is_empty() {
            None
        } else {
            resolve_optional_team_id(&state.pool, Some(tid))
                .await?
                .map(|(id, _)| id)
        };
        assert_host_manage(&auth, &new_team).await?;
        sqlx::query("UPDATE hosts SET team_id = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(&new_team)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    if let Some(labels) = &req.labels {
        let labels_json = serde_json::to_string(labels).unwrap_or_else(|_| "[]".into());
        sqlx::query("UPDATE hosts SET labels = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(&labels_json)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    if let Some(ref host) = req.host {
        sqlx::query("UPDATE hosts SET host = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(host)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    if let Some(port) = req.port {
        sqlx::query("UPDATE hosts SET port = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(port as i64)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    if let Some(ref username) = req.username {
        sqlx::query("UPDATE hosts SET username = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(username)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    if let Some(ref ct) = req.credential_type {
        sqlx::query(
            "UPDATE hosts SET credential_type = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(ct)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }

    if let Some(ref cd) = req.credential_data {
        let encrypted = if cd.is_empty() {
            String::new()
        } else {
            crypto::encrypt(cd, &state.config.auth.jwt_secret)
                .map_err(|e| AppError::Internal(format!("credential encryption failed: {e}")))?
        };
        sqlx::query(
            "UPDATE hosts SET credential_data = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&encrypted)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }

    if let Some(ref pp) = req.credential_passphrase {
        let encrypted = if pp.is_empty() {
            String::new()
        } else {
            crypto::encrypt(pp, &state.config.auth.jwt_secret)
                .map_err(|e| AppError::Internal(format!("passphrase encryption failed: {e}")))?
        };
        sqlx::query(
            "UPDATE hosts SET credential_passphrase = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&encrypted)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }

    if let Some(ref n) = req.name {
        sqlx::query("UPDATE hosts SET name = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(n)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    tracing::info!("Host updated: {id}");

    let after_row = sqlx::query(
        "SELECT name, host, port, username, credential_type, team_id FROM hosts WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;
    let after = after_row.as_ref().map(|r| {
        serde_json::json!({
            "id": id,
            "name": r.get::<String, _>(0),
            "host": r.get::<String, _>(1),
            "port": r.get::<i64, _>(2),
            "username": r.get::<String, _>(3),
            "credential_type": r.get::<String, _>(4),
            "team_id": r.get::<Option<String>, _>(5),
        })
    });
    let name = after_row
        .as_ref()
        .map(|r| r.get::<String, _>(0))
        .unwrap_or(before_name);
    let team_after = after_row
        .as_ref()
        .and_then(|r| r.get::<Option<String>, _>(5));

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::HostUpdate)
            .resource("host", &id, &name)
            .team(team_after)
            .before(before)
            .after(after)
            .meta(
                if req
                    .credential_data
                    .as_ref()
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
                {
                    Some(serde_json::json!({ "credential_rotated": true }))
                } else {
                    None
                },
            ),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "updated": true,
    }))))
}

pub async fn delete(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let team_id = host_team_id(&state.pool, &id).await?;
    assert_host_manage(&auth, &team_id).await?;

    let before_row = sqlx::query(
        "SELECT name, host, port, username, credential_type, team_id FROM hosts WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;
    let (name, before) = match before_row {
        Some(r) => {
            let name: String = r.get(0);
            let before = serde_json::json!({
                "id": id,
                "name": name,
                "host": r.get::<String, _>(1),
                "port": r.get::<i64, _>(2),
                "username": r.get::<String, _>(3),
                "credential_type": r.get::<String, _>(4),
                "team_id": r.get::<Option<String>, _>(5),
            });
            (name, Some(before))
        }
        None => (id.clone(), None),
    };

    let result = sqlx::query("DELETE FROM hosts WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("host not found".into()));
    }

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::HostDelete)
            .resource("host", &id, &name)
            .team(team_id)
            .before(before),
    )
    .await;

    tracing::info!("Host deleted: {id}");
    Ok(Json(ApiResponse::success(())))
}

/// Test SSH connectivity using the Host form values (and optionally stored secrets).
pub async fn test_connection(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<TestHostRequest>,
) -> Result<Json<ApiResponse<TestHostResponse>>, AppError> {
    let host = req.host.trim().to_string();
    let username = req.username.trim().to_string();
    let port = if req.port == 0 { 22 } else { req.port };
    let mut cred_type = req
        .credential_type
        .unwrap_or_else(|| "vault".into())
        .trim()
        .to_string();
    let mut cred_data = req.credential_data.unwrap_or_default();
    let mut passphrase = req.credential_passphrase.unwrap_or_default();

    if let Some(host_id) = req
        .host_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // Probing pulls stored (decryptable) credentials — manage tier, not view.
        let team_id = host_team_id(&state.pool, host_id).await?;
        assert_host_manage(&auth, &team_id).await?;

        let row = sqlx::query(
            "SELECT credential_type, credential_data, credential_passphrase FROM hosts WHERE id = ?",
        )
        .bind(host_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("host not found".into()))?;

        let stored_type: String = row.get(0);
        if cred_type.is_empty() {
            cred_type = stored_type.clone();
        }

        if cred_data.trim().is_empty() && cred_type != "agent" {
            let stored: String = row.get(1);
            if stored.is_empty() {
                return Ok(Json(ApiResponse::success(TestHostResponse {
                    ok: false,
                    latency_ms: 0,
                    message: "no credential stored for this host — paste a key/password to test"
                        .into(),
                })));
            }
            cred_data = crypto::decrypt(&stored, &state.config.auth.jwt_secret).map_err(|e| {
                AppError::ValidationFailed(format!(
                    "stored credential cannot be decrypted ({e}); re-save the host credential"
                ))
            })?;
        }

        if passphrase.trim().is_empty() && cred_type == "key_path" {
            let stored_pp: String = row.get(2);
            if !stored_pp.is_empty() {
                match crypto::decrypt(&stored_pp, &state.config.auth.jwt_secret) {
                    Ok(pp) => passphrase = pp,
                    Err(e) => {
                        tracing::warn!("host probe: passphrase decrypt failed: {e}");
                    }
                }
            }
        }
    } else if !auth.is_platform_admin {
        // Ad-hoc probes reach arbitrary host:port values — an SSRF-style
        // primitive no regular user needs. Save the host first, then test
        // from its card (manage-tier check above).
        return Err(AppError::Unauthorized(
            "ad-hoc connection tests are restricted to platform admins — save the host first and test from its card".into(),
        ));
    }

    if host.is_empty() || username.is_empty() {
        return Ok(Json(ApiResponse::success(TestHostResponse {
            ok: false,
            latency_ms: 0,
            message: "address and username are required".into(),
        })));
    }

    let result = ssh_probe::probe(ProbeParams {
        host,
        port,
        username,
        credential_type: cred_type,
        credential_value: cred_data,
        credential_passphrase: if passphrase.is_empty() {
            None
        } else {
            Some(passphrase)
        },
    })
    .await;

    Ok(Json(ApiResponse::success(TestHostResponse {
        ok: result.ok,
        latency_ms: result.latency_ms,
        message: result.message,
    })))
}
