//! RBAC helpers: AuthUser loading and permission checks.

use shared::auth::{Role, TokenManager};
use shared::error::AppError;
use shared::rbac::{Perm, TeamRole};
use sqlx::{Row, SqlitePool};

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

pub async fn load_auth_user(pool: &SqlitePool, user_id: &str) -> Result<AuthUser, AppError> {
    let row = sqlx::query(
        "SELECT id, username, is_platform_admin, is_super_admin, is_active, is_ops
         FROM users WHERE id = ?",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::AuthFailed("user not found".into()))?;

    let is_active: i64 = row.get(4);
    if is_active == 0 {
        return Err(AppError::AuthFailed("user is disabled".into()));
    }

    let is_platform_admin: i64 = row.get(2);
    let is_super_admin: i64 = row.get(3);
    let memberships = load_memberships(pool, user_id).await?;

    Ok(AuthUser {
        id: row.get(0),
        username: row.get(1),
        is_platform_admin: is_platform_admin != 0,
        is_super_admin: is_super_admin != 0,
        is_ops: is_platform_admin != 0 || row.get::<i64, _>(5) != 0,
        memberships,
        api_token_scope: None,
    })
}

/// Authenticate a bearer credential as a human user.
///
/// Accepts either a user JWT (preferred) or a stored API token (`buz_…`).
/// Returns `AppError::AuthFailed` when neither verifies.
pub async fn authenticate_user_token(
    pool: &SqlitePool,
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
    let row = sqlx::query_as::<_, (String, String)>(
        "SELECT user_id, scope FROM api_tokens WHERE token_hash = ? AND revoked_at IS NULL",
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        tracing::error!("api_token lookup failed: {e}");
        AppError::AuthFailed("invalid or expired token".into())
    })?
    .ok_or_else(|| AppError::AuthFailed("invalid or expired token".into()))?;
    let (user_id, scope_str) = row;

    // Touch last_used_at (best-effort, non-fatal).
    let _ =
        sqlx::query("UPDATE api_tokens SET last_used_at = datetime('now') WHERE token_hash = ?")
            .bind(&token_hash)
            .execute(pool)
            .await;

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
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<TeamMembership>, AppError> {
    let rows = sqlx::query(
        "SELECT tm.team_id, t.slug, t.name, tm.role
         FROM team_members tm
         JOIN teams t ON t.id = tm.team_id
         WHERE tm.user_id = ?
         ORDER BY t.name ASC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let role_str: String = r.get(3);
        let Some(role) = TeamRole::parse(&role_str) else {
            continue;
        };
        out.push(TeamMembership {
            team_id: r.get(0),
            team_slug: r.get(1),
            team_name: r.get(2),
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
    pool: &SqlitePool,
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
    pool: &SqlitePool,
    team_id: Option<&str>,
    owner_group: Option<&str>,
) -> Result<(String, String), AppError> {
    if let Some(tid) = team_id.map(str::trim).filter(|s| !s.is_empty()) {
        let row = sqlx::query("SELECT id, slug FROM teams WHERE id = ?")
            .bind(tid)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| AppError::ValidationFailed("team_id not found".into()))?;
        return Ok((row.get(0), row.get(1)));
    }

    if let Some(slug) = owner_group.map(str::trim).filter(|s| !s.is_empty()) {
        let row = sqlx::query("SELECT id, slug FROM teams WHERE slug = ?")
            .bind(slug)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| {
                AppError::ValidationFailed(format!(
                    "unknown team slug '{slug}' — create the team first or pass team_id"
                ))
            })?;
        return Ok((row.get(0), row.get(1)));
    }

    // Zero-config fallback: default team (created by bootstrap). Matches YAML
    // scanner behavior (`team:` omitted → `default`).
    let row = sqlx::query("SELECT id, slug FROM teams WHERE slug = 'default'")
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| {
            AppError::Internal("default team not found — run manager bootstrap".into())
        })?;
    Ok((row.get(0), row.get(1)))
}

pub async fn resolve_optional_team_id(
    pool: &SqlitePool,
    team_id: Option<&str>,
) -> Result<Option<(String, String)>, AppError> {
    let Some(tid) = team_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let row = sqlx::query("SELECT id, slug FROM teams WHERE id = ?")
        .bind(tid)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::ValidationFailed("team_id not found".into()))?;
    Ok(Some((row.get(0), row.get(1))))
}

pub async fn pipeline_team_id(pool: &SqlitePool, pipeline_id: &str) -> Result<String, AppError> {
    let row = sqlx::query("SELECT team_id FROM pipelines WHERE id = ?")
        .bind(pipeline_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
    let team_id: String = row.get(0);
    if team_id.is_empty() {
        return Err(AppError::Internal(
            "pipeline has no team_id — run migration/bootstrap".into(),
        ));
    }
    Ok(team_id)
}

pub async fn secret_team_id(pool: &SqlitePool, secret_id: &str) -> Result<String, AppError> {
    let row = sqlx::query("SELECT team_id FROM secrets WHERE id = ?")
        .bind(secret_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("secret not found".into()))?;
    let team_id: String = row.get(0);
    if team_id.is_empty() {
        return Err(AppError::Internal(
            "secret has no team_id — run migration/bootstrap".into(),
        ));
    }
    Ok(team_id)
}

pub async fn host_team_id(pool: &SqlitePool, host_id: &str) -> Result<Option<String>, AppError> {
    let row = sqlx::query("SELECT team_id FROM hosts WHERE id = ?")
        .bind(host_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("host not found".into()))?;
    Ok(row.get::<Option<String>, _>(0))
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
pub async fn bootstrap(pool: &SqlitePool) -> anyhow::Result<()> {
    // Ensure a default team exists for migration of empty owner_group rows
    let default_team_id = ensure_team(pool, "default", "Default").await?;

    // Platform admin(s) become owners of the default team (for /me visibility)
    let admin_ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE is_platform_admin = 1 AND is_active = 1")
            .fetch_all(pool)
            .await?;
    for admin_id in &admin_ids {
        sqlx::query(
            "INSERT OR IGNORE INTO team_members (team_id, user_id, role) VALUES (?, ?, 'owner')",
        )
        .bind(&default_team_id)
        .bind(admin_id)
        .execute(pool)
        .await?;
    }

    // Create teams from distinct non-empty owner_group values
    migrate_owner_groups_to_teams(pool).await?;

    // Backfill pipelines.team_id
    backfill_team_id(pool, "pipelines", &default_team_id).await?;
    backfill_team_id(pool, "secrets", &default_team_id).await?;

    Ok(())
}

async fn ensure_team(pool: &SqlitePool, slug: &str, name: &str) -> anyhow::Result<String> {
    if let Some(id) = sqlx::query_scalar::<_, String>("SELECT id FROM teams WHERE slug = ?")
        .bind(slug)
        .fetch_optional(pool)
        .await?
    {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO teams (id, name, slug, description) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(name)
        .bind(slug)
        .bind("")
        .execute(pool)
        .await?;
    tracing::info!("Created team '{name}' (slug={slug})");
    Ok(id)
}

async fn migrate_owner_groups_to_teams(pool: &SqlitePool) -> anyhow::Result<()> {
    let groups: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT owner_group FROM (
            SELECT owner_group FROM pipelines WHERE owner_group != ''
            UNION
            SELECT owner_group FROM secrets WHERE owner_group != ''
         )",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for slug in groups {
        let name = slug.clone();
        ensure_team(pool, &slug, &name).await?;
    }
    Ok(())
}

async fn backfill_team_id(
    pool: &SqlitePool,
    table: &str,
    default_team_id: &str,
) -> anyhow::Result<()> {
    // Match by owner_group slug
    let sql = format!(
        "UPDATE {table} SET team_id = (
            SELECT t.id FROM teams t WHERE t.slug = {table}.owner_group
         )
         WHERE (team_id IS NULL OR team_id = '')
           AND owner_group != ''
           AND EXISTS (SELECT 1 FROM teams t WHERE t.slug = {table}.owner_group)"
    );
    let _ = sqlx::query(&sql).execute(pool).await;

    // Remaining empty → default team; sync owner_group to default slug
    let sql2 = format!(
        "UPDATE {table} SET team_id = ?, owner_group = 'default'
         WHERE team_id IS NULL OR team_id = ''"
    );
    let result = sqlx::query(&sql2)
        .bind(default_team_id)
        .execute(pool)
        .await?;
    if result.rows_affected() > 0 {
        tracing::info!(
            "Backfilled {} rows in {table} to default team",
            result.rows_affected()
        );
    }
    Ok(())
}
