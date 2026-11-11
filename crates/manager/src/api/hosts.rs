use axum::{
    extract::{Path, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateHostRequest, TestHostRequest, TestHostResponse, UpdateHostRequest};
use shared::rbac::Perm;
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent};
use crate::crypto;
use crate::middleware::AuthUser;
use crate::rbac::{host_team_id, resolve_optional_team_id};
use crate::ssh_probe::{self, ProbeParams};
use crate::state::AppState;

fn host_json(h: &crate::store::hosts::HostRow) -> serde_json::Value {
    serde_json::json!({
        "id": h.id,
        "name": h.name,
        "host": h.host,
        "port": h.port as u16,
        "username": h.username,
        "credential_type": h.credential_type,
        "credential_stored": !h.credential_data.is_empty(),
        "labels": serde_json::from_str::<Vec<String>>(&h.labels).unwrap_or_default(),
        "created_at": h.created_at,
        "updated_at": h.updated_at,
        "team_id": h.team_id,
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
    let visible = if auth.is_platform_admin {
        None
    } else {
        Some(auth.team_ids())
    };
    let rows = crate::store::hosts::list(&state.pool, visible.as_deref()).await?;

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

    crate::store::hosts::insert(
        &state.pool,
        &id,
        &req.name,
        &req.host,
        req.port as i64,
        &req.username,
        &cred_type,
        &encrypted_cred_data,
        &encrypted_passphrase,
        &labels_json,
        team_id_opt.as_deref(),
    )
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

    let h = crate::store::hosts::find_full_by_id(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("host not found".into()))?;

    Ok(Json(ApiResponse::success(host_json(&h))))
}

pub async fn update(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateHostRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let current_team = host_team_id(&state.pool, &id).await?;
    assert_host_manage(&auth, &current_team).await?;

    let before_facts = crate::store::hosts::facts(&state.pool, &id).await?;
    let before = before_facts.as_ref().map(|f| {
        serde_json::json!({
            "id": id,
            "name": f.name,
            "host": f.host,
            "port": f.port,
            "username": f.username,
            "credential_type": f.credential_type,
            "team_id": f.team_id,
        })
    });
    let before_name = before_facts
        .as_ref()
        .map(|f| f.name.clone())
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
        crate::store::hosts::update_team_id(&state.pool, &id, new_team.as_deref()).await?;
    }

    if let Some(labels) = &req.labels {
        let labels_json = serde_json::to_string(labels).unwrap_or_else(|_| "[]".into());
        crate::store::hosts::update_labels(&state.pool, &id, &labels_json).await?;
    }

    if let Some(ref host) = req.host {
        crate::store::hosts::update_address(&state.pool, &id, host).await?;
    }

    if let Some(port) = req.port {
        crate::store::hosts::update_port(&state.pool, &id, port as i64).await?;
    }

    if let Some(ref username) = req.username {
        crate::store::hosts::update_username(&state.pool, &id, username).await?;
    }

    if let Some(ref ct) = req.credential_type {
        crate::store::hosts::update_credential_type(&state.pool, &id, ct).await?;
    }

    if let Some(ref cd) = req.credential_data {
        let encrypted = if cd.is_empty() {
            String::new()
        } else {
            crypto::encrypt(cd, &state.config.auth.jwt_secret)
                .map_err(|e| AppError::Internal(format!("credential encryption failed: {e}")))?
        };
        crate::store::hosts::update_credential_data(&state.pool, &id, &encrypted).await?;
    }

    if let Some(ref pp) = req.credential_passphrase {
        let encrypted = if pp.is_empty() {
            String::new()
        } else {
            crypto::encrypt(pp, &state.config.auth.jwt_secret)
                .map_err(|e| AppError::Internal(format!("passphrase encryption failed: {e}")))?
        };
        crate::store::hosts::update_credential_passphrase(&state.pool, &id, &encrypted).await?;
    }

    if let Some(ref n) = req.name {
        crate::store::hosts::update_name(&state.pool, &id, n).await?;
    }

    tracing::info!("Host updated: {id}");

    let after_facts = crate::store::hosts::facts(&state.pool, &id).await?;
    let after = after_facts.as_ref().map(|f| {
        serde_json::json!({
            "id": id,
            "name": f.name,
            "host": f.host,
            "port": f.port,
            "username": f.username,
            "credential_type": f.credential_type,
            "team_id": f.team_id,
        })
    });
    let name = after_facts
        .as_ref()
        .map(|f| f.name.clone())
        .unwrap_or(before_name);
    let team_after = after_facts.as_ref().and_then(|f| f.team_id.clone());

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

    let facts = crate::store::hosts::facts(&state.pool, &id).await?;
    let (name, before) = match facts {
        Some(f) => {
            let name = f.name.clone();
            let before = serde_json::json!({
                "id": id,
                "name": name,
                "host": f.host,
                "port": f.port,
                "username": f.username,
                "credential_type": f.credential_type,
                "team_id": f.team_id,
            });
            (name, Some(before))
        }
        None => (id.clone(), None),
    };

    let deleted = crate::store::hosts::delete(&state.pool, &id).await?;

    if !deleted {
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

        let stored_creds = crate::store::hosts::credentials_of(&state.pool, host_id)
            .await?
            .ok_or_else(|| AppError::NotFound("host not found".into()))?;

        let stored_type: String = stored_creds.credential_type;
        if cred_type.is_empty() {
            cred_type = stored_type.clone();
        }

        if cred_data.trim().is_empty() && cred_type != "agent" {
            let stored: String = stored_creds.credential_data;
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
            let stored_pp: String = stored_creds.credential_passphrase;
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
        return Err(AppError::Forbidden(
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
