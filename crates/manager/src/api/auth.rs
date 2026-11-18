use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Redirect},
    Json,
};
use shared::auth::{Role, TokenManager};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{
    ChangePasswordRequest, LoginRequest, LoginResponse, SetupAdminRequest, SetupStatusResponse,
};
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent, Resource};
use crate::middleware::AuthUser;
use crate::password::{hash_password, validate_password_complexity, verify_password};
use crate::state::AppState;

pub async fn setup_status(
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<SetupStatusResponse>>, AppError> {
    let count = crate::store::auth::count_users(&state.pool).await?;
    Ok(Json(ApiResponse::success(SetupStatusResponse {
        needs_setup: count == 0,
    })))
}

/// Validate first-admin setup fields.
pub fn validate_setup(req: &SetupAdminRequest) -> Result<(), AppError> {
    let username = req.username.trim();
    if username.is_empty() {
        return Err(AppError::ValidationFailed("username is required".into()));
    }
    if username.len() < 2 || username.len() > 64 {
        return Err(AppError::ValidationFailed(
            "username must be 2–64 characters".into(),
        ));
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(AppError::ValidationFailed(
            "username may only contain letters, digits, _ - .".into(),
        ));
    }
    if let Err(msg) = validate_password_complexity(&req.password) {
        return Err(AppError::ValidationFailed(msg));
    }
    Ok(())
}

/// Create the first platform admin when no users exist. Returns a login JWT.
pub async fn setup(
    State(state): State<AppState>,
    Json(req): Json<SetupAdminRequest>,
) -> Result<Json<ApiResponse<LoginResponse>>, AppError> {
    validate_setup(&req)?;
    let username = req.username.trim().to_string();

    // BEGIN IMMEDIATE: setup is a one-shot write whose re-check must not be
    // raced. The dispatcher's DbTx owns the dialect token.
    let db = state.db();
    let mut tx = db.begin_immediate().await?;
    // Serialise first-admin creation on both backends: SQLite holds the
    // BEGIN IMMEDIATE writer lock; Postgres takes a transaction-scoped
    // advisory lock (there is no user row to lock yet).
    crate::store::locks::lock_first_admin_setup_tx(&mut tx).await?;

    let count = crate::store::auth::count_users_tx(&mut tx).await?;
    if count > 0 {
        return Err(AppError::Conflict(
            "setup already completed — use login".into(),
        ));
    }

    let user_id = Uuid::new_v4().to_string();
    let hash = hash_password(&req.password).map_err(AppError::Internal)?;
    crate::store::auth::insert_first_admin_tx(&mut tx, &user_id, &username, &hash).await?;

    // Attach to default team (created by bootstrap)
    if let Some(tid) = crate::store::auth::default_team_id_tx(&mut tx).await? {
        crate::store::auth::attach_team_owner_tx(&mut tx, &tid, &user_id).await?;
    }

    // No actor exists yet: the first admin is a system-provisioned account.
    audit::record_in_tx(
        &mut tx,
        AuditEvent::system(Action::AuthSetup)
            .on(Resource::user(&user_id, &username))
            .after(Some(serde_json::json!({
                "username": username,
                "is_super_admin": true,
            }))),
    )
    .await?;
    tx.commit().await?;

    tracing::info!("First platform admin created via web setup: {username}");

    let token_mgr = TokenManager::new(&state.config.auth.jwt_secret);
    let token = token_mgr
        .create_token(&user_id, Role::Admin, state.config.auth.jwt_ttl_hours)
        .map_err(|e| AppError::Internal(format!("token generation failed: {e}")))?;

    Ok(Json(ApiResponse::success(LoginResponse {
        token,
        user_id: Some(user_id),
        username: Some(username),
        is_platform_admin: Some(true),
    })))
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<ApiResponse<LoginResponse>>, AppError> {
    let user_count = crate::store::auth::count_users(&state.pool).await?;
    if user_count == 0 {
        return Err(AppError::AuthFailed(
            "instance not initialized — open /setup to create the first admin".into(),
        ));
    }

    let username = req.username.trim();
    if username.is_empty() || req.password.is_empty() {
        return Err(AppError::AuthFailed("invalid username or password".into()));
    }

    let user = crate::store::auth::find_login_by_username(&state.pool, username)
        .await?
        .ok_or_else(|| AppError::AuthFailed("invalid username or password".into()))?;

    if !user.is_active {
        return Err(AppError::AuthFailed("user is disabled".into()));
    }

    // Account-level authentication origin: an SSO account must sign in
    // through the provider. The refusal is deliberately identical to every
    // other login failure — the response reveals nothing about which origins
    // an account has (see docs platform/oidc-sso.md).
    if user.auth_source != "local" || user.password_login_disabled {
        return Err(AppError::AuthFailed("invalid username or password".into()));
    }

    if !verify_password(&req.password, &user.password_hash) {
        tracing::warn!(
            username = %username,
            hash_prefix = %user.password_hash.chars().take(20).collect::<String>(),
            "login rejected: password verify failed"
        );
        return Err(AppError::AuthFailed("invalid username or password".into()));
    }

    let role = if user.is_platform_admin {
        Role::Admin
    } else if user.is_ops {
        Role::Ops
    } else {
        Role::User
    };

    let token_mgr = TokenManager::new(&state.config.auth.jwt_secret);
    let token = token_mgr
        .create_token(&user.id, role, state.config.auth.jwt_ttl_hours)
        .map_err(|e| AppError::Internal(format!("token generation failed: {e}")))?;

    audit::record(
        &state.pool,
        AuditEvent::actor(&user.id, &user.username, Action::AuthLogin)
            .on(Resource::user(&user.id, &user.username))
            .after(Some(serde_json::json!({
                "username": user.username.clone(),
                "is_platform_admin": user.is_platform_admin,
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(LoginResponse {
        token,
        user_id: Some(user.id),
        username: Some(user.username),
        is_platform_admin: Some(user.is_platform_admin),
    })))
}

pub async fn verify(auth: AuthUser) -> Result<Json<ApiResponse<()>>, AppError> {
    let _ = auth;
    Ok(Json(ApiResponse::success(())))
}

pub async fn me(auth: AuthUser) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let teams: Vec<serde_json::Value> = auth
        .memberships
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.team_id,
                "slug": m.team_slug,
                "name": m.team_name,
                "role": m.role.as_str(),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": auth.id,
        "username": auth.username,
        "is_platform_admin": auth.is_platform_admin,
        "is_super_admin": auth.is_super_admin,
        "is_ops": auth.is_ops,
        "teams": teams,
    }))))
}

/// Validate change-password fields before hitting the database.
pub fn validate_change_password(req: &ChangePasswordRequest) -> Result<(), AppError> {
    if req.current_password.is_empty() {
        return Err(AppError::ValidationFailed(
            "current_password is required".into(),
        ));
    }
    if req.new_password.is_empty() {
        return Err(AppError::ValidationFailed(
            "new_password is required".into(),
        ));
    }
    if let Err(msg) = validate_password_complexity(&req.new_password) {
        return Err(AppError::ValidationFailed(msg));
    }
    if req.current_password == req.new_password {
        return Err(AppError::ValidationFailed(
            "new_password must differ from current_password".into(),
        ));
    }
    Ok(())
}

pub async fn change_password(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    validate_change_password(&req)?;

    let password_hash = crate::store::auth::password_hash_of(&state.pool, &auth.id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    // JIT-provisioned SSO accounts carry no local password: refusing here
    // keeps them from acquiring one through the change endpoint (an account
    // keeps exactly one authentication origin — see oidc-sso docs).
    if password_hash.is_empty() {
        return Err(AppError::ValidationFailed(
            "this account signs in through SSO and has no local password".into(),
        ));
    }

    if !verify_password(&req.current_password, &password_hash) {
        return Err(AppError::ValidationFailed(
            "current password is incorrect".into(),
        ));
    }

    let new_hash = hash_password(&req.new_password).map_err(AppError::Internal)?;
    crate::store::auth::update_password(&state.pool, &auth.id, &new_hash).await?;
    // The hash never enters the record; the event alone is the signal.
    audit::record(
        &state.pool,
        AuditEvent::new(Action::AuthChangePassword, &auth)
            .on(Resource::user(&auth.id, &auth.username)),
    )
    .await;

    Ok(Json(ApiResponse::success(())))
}

// ───────────────────────────── OIDC single sign-on ─────────────────────────────
//
// Four public endpoints (`providers`, `start`, `callback`, `exchange`) carry
// the browser round trip designed in `docs platform/oidc-sso.md`:
//
//   start    — create a flow (hashed state, nonce, PKCE) and 303 to the IdP
//   callback — consume the flow, exchange + validate the id_token, resolve or
//              JIT-provision the account, hand the SPA a 60-second ticket in
//              an HttpOnly cookie
//   exchange — consume the ticket and answer with the standard LoginResponse
//
// Every refusal lands back on the SPA callback page with a short error code;
// details stay in the server log. No token, code, or secret ever reaches the
// browser except the Buz JWT issued by `exchange`.

/// Cookie carrying the single-use sign-in ticket from `callback` to
/// `exchange`. Scoped to the OIDC path so it is not broadcast to the app.
pub const SSO_TICKET_COOKIE: &str = "buz_sso_ticket";

/// SPA route the callback redirects through.
pub const SSO_CALLBACK_PAGE: &str = "/auth/sso/callback";

/// `GET /auth/oidc/providers` — public capability probe for the login page.
pub async fn oidc_providers(State(state): State<AppState>) -> Json<ApiResponse<serde_json::Value>> {
    Json(ApiResponse::success(serde_json::json!({
        "enabled": state.oidc.is_some(),
    })))
}

/// One browser-facing failure: bounce to the SPA callback page with a short
/// code. The reason stays in the server log, not the URL.
fn sso_redirect_error(error: &'static str) -> axum::response::Response {
    Redirect::to(&format!("{SSO_CALLBACK_PAGE}?error={error}")).into_response()
}

/// Same-site relative path check for the `redirect` query parameter: only
/// `/<path>` forms are accepted (no `//`, no scheme, no backslashes) — the
/// post-login destination cannot become an open redirect.
fn sanitize_redirect(raw: Option<&str>) -> String {
    match raw.map(str::trim) {
        Some(r) if r.starts_with('/') && !r.starts_with("//") && !r.contains('\\') => r.to_string(),
        _ => "/dashboard".into(),
    }
}

/// `GET /auth/oidc/start?redirect=/dashboard` — begin one sign-in flow.
pub async fn oidc_start(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<axum::response::Response, AppError> {
    let Some(runtime) = state.oidc.as_ref() else {
        return Ok(sso_redirect_error("sso-disabled"));
    };
    let redirect_to = sanitize_redirect(params.get("redirect").map(String::as_str));

    // Retention sweep rides on flow creation — the tables are one row per
    // browser navigation, so this never accumulates.
    let _ = crate::store::sso::sweep(&state.pool).await;

    let state_token = crate::services::oidc::random_b64url(32);
    let nonce = crate::services::oidc::random_b64url(32);
    let code_verifier = crate::services::oidc::random_b64url(64);
    let challenge = crate::services::oidc::s256_challenge(&code_verifier);
    let state_hash = crate::services::oidc::sha256_hex(&state_token);
    let flow_id = Uuid::new_v4().to_string();
    let expires_at = crate::store::sso::expires_at_from_now(crate::store::sso::FLOW_TTL_SECONDS);
    crate::store::sso::insert_flow(
        &state.pool,
        &flow_id,
        &state_hash,
        &nonce,
        &code_verifier,
        &redirect_to,
        &expires_at,
    )
    .await?;

    let authorization_endpoint = match runtime.authorization_endpoint().await {
        Ok(endpoint) => endpoint,
        Err(error) => {
            tracing::warn!(error = %error, "oidc start: provider unreachable");
            return Ok(sso_redirect_error("provider-unreachable"));
        }
    };
    let url = runtime.authorize_url(&authorization_endpoint, &state_token, &nonce, &challenge);
    Ok(Redirect::to(&url).into_response())
}

#[derive(Debug, serde::Deserialize)]
pub struct OidcCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    #[serde(rename = "error_description")]
    pub error_description: Option<String>,
}

/// `GET /auth/oidc/callback` — the provider sends the browser back here.
pub async fn oidc_callback(
    State(state): State<AppState>,
    Query(params): Query<OidcCallbackQuery>,
) -> Result<axum::response::Response, AppError> {
    // The provider reports an upstream failure (cancelled sign-in, consent
    // refused, MFA failure…): surface a short code, log the description.
    if let Some(error) = params.error {
        tracing::warn!(
            error = %error,
            description = %params.error_description.unwrap_or_default(),
            "oidc callback: provider returned an error"
        );
        return Ok(sso_redirect_error("provider-error"));
    }

    let Some(runtime) = state.oidc.as_ref() else {
        return Ok(sso_redirect_error("sso-disabled"));
    };
    let Some(state_token) = params.state.as_deref() else {
        return Ok(sso_redirect_error("invalid-state"));
    };
    let Some(code) = params.code.as_deref() else {
        return Ok(sso_redirect_error("missing-code"));
    };

    // Single-use consumption: a replayed/expired state finds nothing.
    let state_hash = crate::services::oidc::sha256_hex(state_token);
    let Some(flow) = crate::store::sso::consume_flow(&state.pool, &state_hash).await? else {
        tracing::warn!("oidc callback: unknown, expired, or replayed state");
        return Ok(sso_redirect_error("invalid-state"));
    };

    // Exchange + validate. Upstream/validation details stay in the log.
    let identity = match runtime.redeem(code, &flow.code_verifier, &flow.nonce).await {
        Ok(identity) => identity,
        Err(error) => {
            tracing::warn!(error = %error, "oidc callback: token validation failed");
            return Ok(sso_redirect_error("invalid-token"));
        }
    };

    // Resolve the account: bound identity, or JIT-provision when policy
    // allows (never for an uninitialized instance — /setup owns first admin).
    let provisioned =
        crate::store::external_identities::find_by_subject(&state.pool, "oidc", &identity.subject)
            .await?;
    let (user, provisioned_now) = match provisioned {
        Some(bound) => {
            let user = match crate::rbac::load_auth_user(&state.pool, &bound.user_id).await {
                Ok(user) => user,
                Err(error) => {
                    // Disabled (or vanished) accounts refuse at resolution —
                    // the IdP identity alone never resurrects an account.
                    tracing::warn!(error = %error, "oidc callback: account refused");
                    return Ok(sso_redirect_error("account-refused"));
                }
            };
            crate::store::external_identities::touch_login(
                &state.pool,
                &bound.id,
                identity.email.as_deref().unwrap_or(""),
                identity.display_name.as_deref().unwrap_or(""),
                &serde_json::to_string(&identity.groups).unwrap_or_else(|_| "[]".into()),
            )
            .await?;
            (user, false)
        }
        None => {
            if !runtime.auto_provision {
                tracing::info!(
                    subject = %identity.subject,
                    "oidc sign-in refused: identity is not bound and auto-provision is off"
                );
                return Ok(sso_redirect_error("not-linked"));
            }
            if crate::store::auth::count_users(&state.pool).await? == 0 {
                // First admin belongs to /setup; a JIT user must never take
                // that slot (or lock the instance out of its own setup).
                tracing::warn!("oidc sign-in refused: instance is not initialized");
                return Ok(sso_redirect_error("not-initialized"));
            }
            let user = match provision_oidc_user(&state, &identity).await {
                Ok(user) => user,
                Err(error) => {
                    tracing::warn!(error = %error, "oidc provisioning failed");
                    return Ok(sso_redirect_error("provision-failed"));
                }
            };
            (user, true)
        }
    };

    let (action, after) = if provisioned_now {
        (
            audit::Action::AuthOidcProvision,
            serde_json::json!({
                "username": user.username,
                "subject": identity.subject,
                "email": identity.email,
            }),
        )
    } else {
        (
            audit::Action::AuthOidcLogin,
            serde_json::json!({
                "username": user.username,
                "subject": identity.subject,
            }),
        )
    };
    audit::record(
        &state.pool,
        audit::AuditEvent::actor(&user.id, &user.username, action)
            .on(audit::Resource::user(&user.id, &user.username))
            .after(Some(after)),
    )
    .await;

    // 60-second single-use ticket for the SPA to exchange for the real JWT.
    let ticket = crate::services::oidc::random_b64url(32);
    let ticket_hash = crate::services::oidc::sha256_hex(&ticket);
    let expires_at = crate::store::sso::expires_at_from_now(crate::store::sso::TICKET_TTL_SECONDS);
    crate::store::sso::insert_ticket(&state.pool, &ticket_hash, &user.id, &expires_at).await?;

    let next = crate::services::oidc::percent_encode(&flow.redirect_to);
    let mut response = Redirect::to(&format!("{SSO_CALLBACK_PAGE}?next={next}")).into_response();
    let cookie = format!(
        "{SSO_TICKET_COOKIE}={ticket}; HttpOnly; SameSite=Lax; Path=/api/v1/auth/oidc; Max-Age={}{}",
        crate::store::sso::TICKET_TTL_SECONDS,
        secure_suffix(&state)
    );
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response
            .headers_mut()
            .append(axum::http::header::SET_COOKIE, value);
    }
    Ok(response)
}

/// Derive a free username from the identity claims: email local-part, then
/// display name, then a generic base — sanitized, bounded, deconflicted with
/// numeric suffixes against the UNIQUE users.username column.
async fn provision_oidc_user(
    state: &AppState,
    identity: &crate::services::oidc::OidcIdentity,
) -> Result<AuthUser, AppError> {
    let raw_source = identity
        .email
        .as_deref()
        .and_then(|e| e.split('@').next())
        .filter(|s| !s.is_empty())
        .or(identity.display_name.as_deref())
        .unwrap_or("sso-user");
    let sanitized: String = raw_source
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    let base = if sanitized.is_empty() {
        "sso-user".to_string()
    } else {
        sanitized.chars().take(48).collect()
    };

    let mut username = base.clone();
    let mut suffix = 2u32;
    loop {
        let taken = crate::store::auth::find_login_by_username(&state.pool, &username)
            .await?
            .is_some();
        if !taken {
            break;
        }
        username = format!("{base}-{suffix}");
        suffix += 1;
    }

    let user_id = Uuid::new_v4().to_string();
    crate::store::users::insert_oidc(
        &state.pool,
        &user_id,
        &username,
        identity.display_name.as_deref().unwrap_or(&username),
    )
    .await?;
    crate::store::external_identities::insert(
        &state.pool,
        &Uuid::new_v4().to_string(),
        "oidc",
        &state.config.auth.oidc.issuer,
        &identity.subject,
        &user_id,
        identity.email.as_deref().unwrap_or(""),
        identity.display_name.as_deref().unwrap_or(""),
        &serde_json::to_string(&identity.groups).unwrap_or_else(|_| "[]".into()),
    )
    .await?;

    crate::rbac::load_auth_user(&state.pool, &user_id).await
}

/// `POST /auth/oidc/exchange` — consume the HttpOnly ticket cookie and answer
/// with the same `LoginResponse` a local login returns.
pub async fn oidc_exchange(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, AppError> {
    let ticket = headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .map(|pair| pair.trim())
        .find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name.trim() == SSO_TICKET_COOKIE).then(|| value.trim().to_string())
        })
        .ok_or_else(|| AppError::AuthFailed("invalid or expired ticket".into()))?;

    let ticket_hash = crate::services::oidc::sha256_hex(&ticket);
    let Some(user_id) = crate::store::sso::consume_ticket(&state.pool, &ticket_hash).await? else {
        return Err(AppError::AuthFailed("invalid or expired ticket".into()));
    };

    let user = crate::rbac::load_auth_user(&state.pool, &user_id).await?;
    let role = role_for(user.is_platform_admin, user.is_ops);
    let token_mgr = TokenManager::new(&state.config.auth.jwt_secret);
    let token = token_mgr
        .create_token(&user.id, role, state.config.auth.jwt_ttl_hours)
        .map_err(|e| AppError::Internal(format!("token generation failed: {e}")))?;

    let mut response = Json(ApiResponse::success(LoginResponse {
        token,
        user_id: Some(user.id.clone()),
        username: Some(user.username.clone()),
        is_platform_admin: Some(user.is_platform_admin),
    }))
    .into_response();
    // Handoff is complete — the ticket must not linger in the browser.
    if let Ok(value) = HeaderValue::from_str(&format!(
        "{SSO_TICKET_COOKIE}=; HttpOnly; SameSite=Lax; Path=/api/v1/auth/oidc; Max-Age=0{}",
        secure_suffix(&state)
    )) {
        response
            .headers_mut()
            .append(axum::http::header::SET_COOKIE, value);
    }
    Ok(response)
}

fn role_for(is_platform_admin: bool, is_ops: bool) -> Role {
    if is_platform_admin {
        Role::Admin
    } else if is_ops {
        Role::Ops
    } else {
        Role::User
    }
}

/// `Secure` only when Buz terminates TLS itself; behind a TLS-terminating
/// proxy the cookie is still HttpOnly + SameSite=Lax + path-scoped.
fn secure_suffix(state: &AppState) -> &'static str {
    if state.config.server.tls_enabled() {
        "; Secure"
    } else {
        ""
    }
}
