//! Administrator queue for identities refused at sign-in
//! (`platform/oidc-sso.md`): with `auto_provision = false` the callback
//! records the refused identity and this module lets a platform admin
//! approve it — bind to an existing account, or create a plain SSO user —
//! or dismiss it.
//!
//! Authority note: a platform admin can already reset any user's password,
//! so binding an identity on their behalf grants no new power; the binding
//! is an explicit admin assertion on a verified `sub`, never an email match.
//! Disabling the target's local password is a separate, explicit choice.

use axum::{extract::Path, extract::State, Json};
use serde::Deserialize;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent, Resource};
use crate::middleware::PlatformAdmin;
use crate::state::AppState;

/// `GET /auth/oidc/identities/pending` — the queue, oldest attempt first.
pub async fn list_pending(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let rows = crate::store::pending_identities::list(&state.pool).await?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "id": row.id,
                "provider": row.provider,
                "issuer": row.issuer,
                "subject": row.subject,
                "email": row.email,
                "display_name": row.display_name,
                "groups": serde_json::from_str::<Vec<String>>(&row.groups_json).unwrap_or_default(),
                "first_seen_at": row.first_seen_at,
                "last_seen_at": row.last_seen_at,
                "attempts": row.attempts,
            })
        })
        .collect();
    Ok(Json(ApiResponse::success(serde_json::json!({
        "items": items,
    }))))
}

#[derive(Deserialize)]
pub struct BindPendingIdentityRequest {
    /// Bind to this existing account.
    #[serde(default)]
    pub user_id: Option<String>,
    /// Or create a plain SSO user (username derived from the identity when
    /// omitted).
    #[serde(default)]
    pub create_username: Option<String>,
    /// Also close local password login on the target account (existing
    /// accounts only; JIT-style accounts have none anyway).
    #[serde(default)]
    pub disable_local_password: bool,
}

/// `POST /auth/oidc/identities/pending/{id}/bind` — approve one identity.
///
/// One transaction does the lookup, the binding, the optional account
/// conversion, and the queue removal; a UNIQUE conflict anywhere rolls the
/// whole thing back and surfaces as a clear refusal.
pub async fn bind_pending(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<BindPendingIdentityRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let runtime_issuer = state
        .oidc_runtime()
        .await
        .map(|runtime| runtime.issuer.clone())
        .unwrap_or_default();

    let mut tx = state.pool.begin_immediate().await?;
    let Some(pending) = crate::store::pending_identities::get_tx(&mut tx, &id).await? else {
        return Err(AppError::NotFound("pending identity not found".into()));
    };

    let (user_id, username, created, password_disabled) = if let Some(user_id) = req
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // Existing account: active, local-origin (SSO accounts are mutually
        // exclusive with a second identity per the account model).
        let Some(facts) = crate::store::users::binding_facts_tx(&mut tx, user_id).await? else {
            return Err(AppError::NotFound("user not found".into()));
        };
        if !facts.is_active {
            return Err(AppError::ValidationFailed(
                "cannot bind to a disabled user".into(),
            ));
        }
        if facts.auth_source != "local" {
            return Err(AppError::ValidationFailed(
                "account already signs in through SSO".into(),
            ));
        }
        if crate::store::external_identities::find_by_user(&mut tx, "oidc", user_id)
            .await?
            .is_some()
        {
            return Err(AppError::Conflict(
                "user already carries an identity for this provider".into(),
            ));
        }
        // Closing local login on a platform admin must not remove the
        // break-glass path.
        let password_disabled = req.disable_local_password;
        if password_disabled && facts.is_platform_admin {
            let remaining =
                crate::store::users::count_active_local_admins_excluding_tx(&mut tx, user_id)
                    .await?;
            if remaining == 0 {
                return Err(AppError::ValidationFailed(
                    "cannot disable the last local platform admin's password login".into(),
                ));
            }
        }
        if password_disabled {
            crate::store::users::convert_to_oidc_tx(&mut tx, user_id).await?;
        }
        (
            user_id.to_string(),
            facts.username,
            false,
            password_disabled,
        )
    } else {
        // New plain SSO user: requested username or derived from the claims.
        // The derivation is the shared one (`services::username`) so an
        // approved identity gets exactly the name JIT provisioning would have
        // given — one policy, two callers, no drift.
        let base = match req.create_username.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => crate::services::username::sanitize_username(name)
                .unwrap_or_else(|| crate::services::username::subject_fallback(&pending.subject)),
            _ => crate::services::username::sanitize_username(
                &crate::services::username::username_base(
                    Some(pending.email.as_str()),
                    Some(pending.display_name.as_str()),
                    &pending.subject,
                ),
            )
            .unwrap_or_else(|| crate::services::username::subject_fallback(&pending.subject)),
        };
        let mut username = base.clone();
        let mut suffix = 2u32;
        loop {
            if !crate::store::auth::username_taken_tx(&mut tx, &username).await? {
                break;
            }
            username = format!("{base}-{suffix}");
            suffix += 1;
        }
        let new_id = Uuid::new_v4().to_string();
        let display = if pending.display_name.is_empty() {
            username.clone()
        } else {
            pending.display_name.clone()
        };
        crate::store::users::insert_oidc_tx(&mut tx, &new_id, &username, &display).await?;
        (new_id, username, true, true)
    };

    crate::store::external_identities::insert_tx(
        &mut tx,
        &Uuid::new_v4().to_string(),
        &pending.provider,
        &pending.issuer,
        &pending.subject,
        &user_id,
        &pending.email,
        &pending.display_name,
        &pending.groups_json,
    )
    .await?;
    crate::store::pending_identities::delete_tx(&mut tx, &id).await?;
    tx.commit().await?;

    audit::record(
        &state.pool,
        AuditEvent::new(Action::AuthOidcIdentityApprove, &auth)
            .on(Resource::user(&user_id, &username))
            .after(Some(serde_json::json!({
                "subject": pending.subject,
                "email": pending.email,
                "issuer": if runtime_issuer.is_empty() { pending.issuer.clone() } else { runtime_issuer },
                "created_user": created,
                "password_login_disabled": password_disabled,
                "detail": format!(
                    "{} account {}",
                    if created { "created" } else { "linked" },
                    username
                ),
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "user_id": user_id,
        "username": username,
        "created_user": created,
        "password_login_disabled": password_disabled,
    }))))
}

/// `DELETE /auth/oidc/identities/pending/{id}` — dismiss one identity. A
/// later sign-in attempt re-records it (the queue is display data, not a
/// blocklist).
pub async fn reject_pending(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let mut tx = state.pool.begin_immediate().await?;
    let Some(pending) = crate::store::pending_identities::get_tx(&mut tx, &id).await? else {
        return Err(AppError::NotFound("pending identity not found".into()));
    };
    crate::store::pending_identities::delete_tx(&mut tx, &id).await?;
    tx.commit().await?;

    audit::record(
        &state.pool,
        AuditEvent::new(Action::AuthOidcIdentityReject, &auth)
            .on(Resource::identity(&pending.subject, &pending.display_name))
            .after(Some(serde_json::json!({
                "subject": pending.subject,
                "email": pending.email,
                "attempts": pending.attempts,
                "detail": format!(
                    "sign-in refused ({} attempt(s))",
                    pending.attempts
                ),
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "dismissed": true,
    }))))
}
