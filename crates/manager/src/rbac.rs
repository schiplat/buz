//! RBAC helpers: AuthUser loading and permission checks.

use shared::auth::{Role, TokenManager};
use shared::error::AppError;
use shared::rbac::{Perm, TeamRole};

use crate::store::exec::DbPool;

#[derive(Debug, Clone)]
pub struct TeamMembership {
    pub team_id: String,
    pub team_slug: String,
    pub team_name: String,
    pub role: TeamRole,
}

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: String,
    pub username: String,
    pub is_platform_admin: bool,
    /// Super admins see all audit logs and all API tokens.
    pub is_super_admin: bool,
    /// Platform-ops user: sandbox host administration via the ops API
    /// namespace. Grants no other platform-admin power.
    pub is_ops: bool,
    pub memberships: Vec<TeamMembership>,
    /// Set only when the request was authenticated with an API token (`buz_…`)
    /// rather than a user JWT. Bounds where the credential may go — see
    /// [`AuthUser::assert_api_token_allowed`].
    pub api_token_scope: Option<shared::api_token::ApiTokenScope>,
}

impl AuthUser {
    pub fn team_ids(&self) -> Vec<String> {
        self.memberships.iter().map(|m| m.team_id.clone()).collect()
    }

    pub fn role_in(&self, team_id: &str) -> Option<TeamRole> {
        self.memberships
            .iter()
            .find(|m| m.team_id == team_id)
            .map(|m| m.role)
    }

    pub fn check_team(&self, team_id: &str, perm: Perm) -> Result<(), AppError> {
        if self.is_platform_admin {
            return Ok(());
        }
        let Some(role) = self.role_in(team_id) else {
            return Err(AppError::Forbidden("not a member of this team".into()));
        };
        if role.at_least(perm.min_role()) {
            Ok(())
        } else {
            Err(AppError::Forbidden(format!(
                "insufficient team role for {:?}",
                perm
            )))
        }
    }

    /// Require team membership with role ≥ `required` (platform admin bypasses).
    pub fn check_team_role(&self, team_id: &str, required: TeamRole) -> Result<(), AppError> {
        if self.is_platform_admin {
            return Ok(());
        }
        let Some(role) = self.role_in(team_id) else {
            return Err(AppError::Forbidden("not a member of this team".into()));
        };
        if role.at_least(required) {
            Ok(())
        } else {
            Err(AppError::Forbidden(format!(
                "requires team role >= {}",
                required.as_str()
            )))
        }
    }

    pub fn require_platform_admin(&self) -> Result<(), AppError> {
        if self.is_platform_admin {
            Ok(())
        } else {
            Err(AppError::Forbidden("platform admin required".into()))
        }
    }

    /// `None` means all teams (platform admin). Empty vec means no access.
    pub fn visible_team_filter(&self) -> Option<Vec<String>> {
        if self.is_platform_admin {
            None
        } else {
            Some(self.team_ids())
        }
    }

    /// Enforce the API-token authority bands on an incoming request.
    ///
    /// JWT sessions pass through untouched (`api_token_scope == None`). API
    /// tokens are operations-plane credentials, so regardless of scope they
    /// may never reach:
    /// - the **identity plane** — user management, token management (no
    ///   self-perpetuation), password change;
    /// - the **governance plane** — platform settings, plugin sources/pool
    ///   curation.
    ///
    /// Within the operations plane, the `read` band is limited to safe
    /// methods (GET/HEAD/OPTIONS); `ops` and the legacy `full` band may use
    /// any method.
    pub fn assert_api_token_allowed(&self, method: &str, path: &str) -> Result<(), AppError> {
        let Some(scope) = self.api_token_scope else {
            return Ok(());
        };

        // Compare the route relative to the API mount (`/api/v1/...`), so the
        // deny list below reads exactly like the router table.
        let rel = path
            .trim_start_matches('/')
            .strip_prefix("api/v1/")
            .unwrap_or_else(|| path.trim_start_matches('/'));

        const IDENTITY_PREFIXES: &[&str] = &["users", "api-tokens", "auth/change-password"];
        const GOVERNANCE_PREFIXES: &[&str] = &[
            "admin/settings",
            "admin/plugin-sources",
            "admin/plugin-pool",
        ];

        let denied = |prefixes: &[&str]| {
            prefixes
                .iter()
                .any(|p| rel == *p || rel.starts_with(&format!("{p}/")))
        };
        if denied(IDENTITY_PREFIXES) {
            return Err(AppError::Forbidden(
                "api tokens cannot access identity endpoints (users, api-tokens, password change) \
                 — use a browser session"
                    .into(),
            ));
        }
        if denied(GOVERNANCE_PREFIXES) {
            return Err(AppError::Forbidden(
                "api tokens cannot access platform governance endpoints (settings, plugin \
                 sources/pool) — use a browser session"
                    .into(),
            ));
        }

        if scope == shared::api_token::ApiTokenScope::Read
            && !matches!(method, "GET" | "HEAD" | "OPTIONS")
        {
            return Err(AppError::Forbidden(
                "read-only api token — create an 'ops' token for write access".into(),
            ));
        }

        Ok(())
    }
}

pub async fn load_auth_user(pool: &DbPool, user_id: &str) -> Result<AuthUser, AppError> {
    let row = crate::store::users::auth_facts(pool, user_id)
        .await?
        .ok_or_else(|| AppError::AuthFailed("user not found".into()))?;

    if !row.is_active {
        return Err(AppError::AuthFailed("user is disabled".into()));
    }

    let memberships = load_memberships(pool, user_id).await?;

    Ok(AuthUser {
        id: row.id,
        username: row.username,
        is_platform_admin: row.is_platform_admin,
        is_super_admin: row.is_super_admin,
        is_ops: row.is_platform_admin || row.is_ops,
        memberships,
        api_token_scope: None,
    })
}

/// Authenticate a bearer credential as a human user.
///
/// Accepts either a user JWT (preferred) or a stored API token (`buz_…`).
/// Returns `AppError::AuthFailed` when neither verifies.
pub async fn authenticate_user_token(
    pool: &DbPool,
    jwt_secret: &str,
    token: &str,
) -> Result<AuthUser, AppError> {
    let tm = TokenManager::new(jwt_secret);
    if let Ok(claims) = tm.verify(token) {
        if !matches!(claims.role, Role::Admin | Role::Ops | Role::User) {
            return Err(AppError::AuthFailed("user role required".into()));
        }
        return load_auth_user(pool, &claims.sub).await;
    }

    // API token authentication: SHA-256 lookup, then load the owning user.
    let token_hash = shared::api_token::hash_token(token);
    let row = crate::store::api_tokens::find_by_hash(pool, &token_hash)
        .await
        .map_err(|e| {
            tracing::error!("api_token lookup failed: {e}");
            AppError::AuthFailed("invalid or expired token".into())
        })?
        .ok_or_else(|| AppError::AuthFailed("invalid or expired token".into()))?;
    let (user_id, scope_str) = row;

    // Touch last_used_at (best-effort, non-fatal).
    crate::store::api_tokens::touch_last_used(pool, &token_hash).await;

    let scope = match shared::api_token::ApiTokenScope::parse(&scope_str) {
        Some(scope) => Some(scope),
        None => {
            tracing::warn!(
                "api token for user {user_id} has unknown scope {scope_str:?} — treating as legacy 'full'"
            );
            Some(shared::api_token::ApiTokenScope::Full)
        }
    };

    let mut user = load_auth_user(pool, &user_id).await?;
    user.api_token_scope = scope;
    Ok(user)
}

pub async fn load_memberships(
    pool: &DbPool,
    user_id: &str,
) -> Result<Vec<TeamMembership>, AppError> {
    let rows = crate::store::users::memberships(pool, user_id).await?;

    let mut out = Vec::with_capacity(rows.len());
    for (team_id, team_slug, team_name, role_str) in rows {
        let Some(role) = TeamRole::parse(&role_str) else {
            continue;
        };
        out.push(TeamMembership {
            team_id,
            team_slug,
            team_name,
            role,
        });
    }
    Ok(out)
}

/// Verify a platform-admin credential for CLI subcommands (e.g. `gen-runner-token`).
///
/// Mirrors the HTTP `PlatformAdmin` middleware: the credential must be a valid
/// user JWT signed with the configured secret **or** a stored API token, belong
/// to a human user who is still active, and that user must hold
/// `is_platform_admin`. Returns the caller's identity on success.
pub async fn verify_cli_admin_token(
    pool: &DbPool,
    jwt_secret: &str,
    token: &str,
) -> Result<AuthUser, AppError> {
    let user = authenticate_user_token(pool, jwt_secret, token).await?;
    if !user.is_platform_admin {
        return Err(AppError::Forbidden("platform admin required".into()));
    }
    Ok(user)
}

/// Resolve team_id from request: prefer `team_id`, else look up slug from `owner_group`.
/// When both are empty, fall back to the `default` team so callers (create secret /
/// pipeline) stay consistent with YAML pipelines whose omitted `team:` also resolves
/// to `default` — the user never has to manually align a team.
pub async fn resolve_required_team_id(
    pool: &DbPool,
    team_id: Option<&str>,
    owner_group: Option<&str>,
) -> Result<(String, String), AppError> {
    if let Some(tid) = team_id.map(str::trim).filter(|s| !s.is_empty()) {
        return crate::store::teams::id_and_slug_by_id(pool, tid)
            .await?
            .ok_or_else(|| AppError::ValidationFailed("team_id not found".into()));
    }

    if let Some(slug) = owner_group.map(str::trim).filter(|s| !s.is_empty()) {
        return crate::store::teams::id_and_slug_by_slug(pool, slug)
            .await?
            .ok_or_else(|| {
                AppError::ValidationFailed(format!(
                    "unknown team slug '{slug}' — create the team first or pass team_id"
                ))
            });
    }

    // Zero-config fallback: default team (created by bootstrap). Matches YAML
    // scanner behavior (`team:` omitted → `default`).
    crate::store::teams::id_and_slug_by_slug(pool, "default")
        .await?
        .ok_or_else(|| AppError::Internal("default team not found — run manager bootstrap".into()))
}

pub async fn resolve_optional_team_id(
    pool: &DbPool,
    team_id: Option<&str>,
) -> Result<Option<(String, String)>, AppError> {
    let Some(tid) = team_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    crate::store::teams::id_and_slug_by_id(pool, tid)
        .await?
        .ok_or_else(|| AppError::ValidationFailed("team_id not found".into()))
        .map(Some)
}

pub async fn pipeline_team_id(pool: &DbPool, pipeline_id: &str) -> Result<String, AppError> {
    let team_id = crate::store::pipelines::team_id_of(pool, pipeline_id)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
    if team_id.is_empty() {
        return Err(AppError::Internal(
            "pipeline has no team_id — run migration/bootstrap".into(),
        ));
    }
    Ok(team_id)
}

pub async fn secret_team_id(pool: &DbPool, secret_id: &str) -> Result<String, AppError> {
    let team_id = crate::store::secrets::team_id_of(pool, secret_id)
        .await?
        .ok_or_else(|| AppError::NotFound("secret not found".into()))?;
    if team_id.is_empty() {
        return Err(AppError::Internal(
            "secret has no team_id — run migration/bootstrap".into(),
        ));
    }
    Ok(team_id)
}

pub async fn host_team_id(pool: &DbPool, host_id: &str) -> Result<Option<String>, AppError> {
    crate::store::hosts::team_id_of(pool, host_id)
        .await?
        .ok_or_else(|| AppError::NotFound("host not found".into()))
}

/// Build `AND team_id IN (?,?,?)` clause + binds. Returns empty filter for platform admin.
pub fn team_in_clause(auth: &AuthUser, column: &str) -> Option<(String, Vec<String>)> {
    match auth.visible_team_filter() {
        None => None,
        Some(ids) if ids.is_empty() => Some(("AND 1=0".into(), vec![])),
        Some(ids) => {
            let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            Some((format!("AND {column} IN ({placeholders})"), ids))
        }
    }
}

/// Bootstrap default team + migrate owner_group → team_id.
/// First platform admin is created via `POST /auth/setup` (web UI), not from config.
pub async fn bootstrap(pool: &DbPool) -> anyhow::Result<()> {
    crate::store::teams::bootstrap(pool)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}
