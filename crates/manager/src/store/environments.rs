//! Platform Environments — named scopes for Variable / Secret default overrides.
//!
//! Shared defaults live in `platform_vars` / `secrets`. An Environment may
//! override a subset of those values for a job when the operator selects it at
//! trigger time. GitHub sync per-repo overrides are a separate axis
//! (`github_sync`).
//!
//! On the dual-backend dispatcher (`store::exec`): statements keep the
//! SQLite dialect and the PG arm rewrites at execution time; projections are
//! fixed, so rows decode by position.

use crate::store::exec::{self, DbPool, SqlValue};

use shared::error::AppError;
use shared::rbac::TeamRole;
use shared::secret_kind::normalize_secret_kind;

use crate::crypto;

/// Protection / approval-gate settings for an Environment (方案 B).
#[derive(Debug, Clone)]
pub struct EnvironmentProtection {
    pub enabled: bool,
    pub min_trigger_role: TeamRole,
    pub min_approve_role: TeamRole,
    pub require_approvals: i64,
    pub forbid_self_approve: bool,
    pub approval_timeout_seconds: i64,
}

impl EnvironmentProtection {
    pub fn defaults() -> Self {
        Self {
            enabled: false,
            min_trigger_role: TeamRole::Developer,
            min_approve_role: TeamRole::Maintainer,
            require_approvals: 1,
            forbid_self_approve: true,
            approval_timeout_seconds: 86400,
        }
    }

    /// Nested JSON frozen into `job.snapshot.protection`.
    pub fn to_snapshot_json(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "min_trigger_role": self.min_trigger_role.as_str(),
            "min_approve_role": self.min_approve_role.as_str(),
            "require_approvals": self.require_approvals,
            "forbid_self_approve": self.forbid_self_approve,
            "approval_timeout_seconds": self.approval_timeout_seconds,
        })
    }

    /// Flat fields for Environment list/get/create/update API responses.
    pub fn to_api_fields(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("protection_enabled".into(), serde_json::json!(self.enabled));
        m.insert(
            "min_trigger_role".into(),
            serde_json::json!(self.min_trigger_role.as_str()),
        );
        m.insert(
            "min_approve_role".into(),
            serde_json::json!(self.min_approve_role.as_str()),
        );
        m.insert(
            "require_approvals".into(),
            serde_json::json!(self.require_approvals),
        );
        m.insert(
            "forbid_self_approve".into(),
            serde_json::json!(self.forbid_self_approve),
        );
        m.insert(
            "approval_timeout_seconds".into(),
            serde_json::json!(self.approval_timeout_seconds),
        );
        m
    }
}

/// Roles allowed for `min_trigger_role` / `min_approve_role` (viewer is never enough).
pub fn parse_protection_role(raw: &str, field: &str) -> Result<TeamRole, AppError> {
    match TeamRole::parse(raw.trim()) {
        Some(role @ (TeamRole::Owner | TeamRole::Maintainer | TeamRole::Developer)) => Ok(role),
        Some(TeamRole::Viewer) => Err(AppError::ValidationFailed(format!(
            "{field} cannot be 'viewer' — use owner, maintainer, or developer"
        ))),
        None => Err(AppError::ValidationFailed(format!(
            "{field} must be owner, maintainer, or developer"
        ))),
    }
}

pub fn protection_from_db_row(
    enabled: i64,
    min_trigger: &str,
    min_approve: &str,
    require: i64,
    forbid: i64,
    timeout: i64,
) -> EnvironmentProtection {
    EnvironmentProtection {
        enabled: enabled != 0,
        min_trigger_role: TeamRole::parse(min_trigger).unwrap_or(TeamRole::Developer),
        min_approve_role: TeamRole::parse(min_approve).unwrap_or(TeamRole::Maintainer),
        require_approvals: require.max(1),
        forbid_self_approve: forbid != 0,
        approval_timeout_seconds: timeout.max(1),
    }
}

pub const ENV_PROTECTION_SELECT: &str = "protection_enabled, min_trigger_role, min_approve_role, \
     require_approvals, forbid_self_approve, approval_timeout_seconds";

pub fn merge_protection_from_request(
    base: &EnvironmentProtection,
    enabled: Option<bool>,
    min_trigger_role: Option<&str>,
    min_approve_role: Option<&str>,
    require_approvals: Option<i64>,
    forbid_self_approve: Option<bool>,
    approval_timeout_seconds: Option<i64>,
) -> Result<EnvironmentProtection, AppError> {
    let mut p = base.clone();
    if let Some(v) = enabled {
        p.enabled = v;
    }
    if let Some(s) = min_trigger_role {
        p.min_trigger_role = parse_protection_role(s, "min_trigger_role")?;
    }
    if let Some(s) = min_approve_role {
        p.min_approve_role = parse_protection_role(s, "min_approve_role")?;
    }
    if let Some(n) = require_approvals {
        if n < 1 {
            return Err(AppError::ValidationFailed(
                "require_approvals must be >= 1".into(),
            ));
        }
        p.require_approvals = n;
    }
    if let Some(v) = forbid_self_approve {
        p.forbid_self_approve = v;
    }
    if let Some(n) = approval_timeout_seconds {
        if n < 1 {
            return Err(AppError::ValidationFailed(
                "approval_timeout_seconds must be >= 1".into(),
            ));
        }
        p.approval_timeout_seconds = n;
    }
    Ok(p)
}

pub fn validate_slug(slug: &str) -> Result<(), AppError> {
    if slug != slug.trim() {
        return Err(AppError::ValidationFailed(
            "slug must not contain surrounding whitespace".into(),
        ));
    }
    if slug.is_empty() {
        return Err(AppError::ValidationFailed("slug is required".into()));
    }
    if slug.len() > 64 {
        return Err(AppError::ValidationFailed(
            "slug must be ≤ 64 characters".into(),
        ));
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(AppError::ValidationFailed(
            "slug must be lowercase letters, digits, '-' or '_'".into(),
        ));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(AppError::ValidationFailed(
            "slug must not start or end with '-'".into(),
        ));
    }
    Ok(())
}

pub fn validate_secret_override_value(value: &str) -> Result<(), AppError> {
    if value.is_empty() {
        return Err(AppError::ValidationFailed(
            "secret override value must be non-empty (delete the override to inherit)".into(),
        ));
    }
    Ok(())
}

/// Validate the deployment concurrency mode value.
pub fn validate_deployment_concurrency(raw: Option<&str>) -> Result<String, AppError> {
    match raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("off")
    {
        "off" => Ok("off".into()),
        "queue" => Ok("queue".into()),
        "cancel_in_progress" => Ok("cancel_in_progress".into()),
        other => Err(AppError::ValidationFailed(format!(
            "deployment_concurrency '{other}' is invalid — use off, queue or cancel_in_progress"
        ))),
    }
}

/// Resolved environment for trigger: id, slug, and protection settings.
pub struct ResolvedEnvironment {
    pub id: String,
    pub slug: String,
    pub protection: EnvironmentProtection,
}

/// Every environment with its override counts, protection and concurrency.
pub async fn list_environments(pool: &DbPool) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT e.id, e.name, e.slug, e.description, e.updated_at,
                (SELECT COUNT(*) FROM environment_var_overrides o WHERE o.environment_id = e.id) AS var_override_count,
                (SELECT COUNT(*) FROM environment_secret_overrides o WHERE o.environment_id = e.id) AS secret_override_count,
                e.protection_enabled, e.min_trigger_role, e.min_approve_role,
                e.require_approvals, e.forbid_self_approve, e.approval_timeout_seconds,
                COALESCE(e.deployment_concurrency, 'off') AS deployment_concurrency,
                (SELECT COUNT(*) FROM environment_hosts h WHERE h.environment_id = e.id) AS host_allowlist_count
         FROM environments e
         ORDER BY e.name ASC",
        &[],
    )
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let var_n = r.i64(5);
            let secret_n = r.i64(6);
            let protection = protection_from_db_row(
                r.i64(7),
                &r.text(8),
                &r.text(9),
                r.i64(10),
                r.i64(11),
                r.i64(12),
            );
            let mut obj = serde_json::json!({
                "id": r.text(0),
                "name": r.text(1),
                "slug": r.text(2),
                "description": r.text(3),
                "updated_at": r.opt_text(4),
                "var_override_count": var_n,
                "secret_override_count": secret_n,
                // Backward-compatible total for list badges.
                "override_count": var_n + secret_n,
                "deployment_concurrency": r.text(13),
                "host_allowlist_count": r.i64(14),
            });
            if let Some(map) = obj.as_object_mut() {
                map.extend(protection.to_api_fields());
            }
            obj
        })
        .collect())
}

/// One environment's detail payload. `include_var_overrides` = platform admin:
/// variable overrides carry plaintext and are hidden from everyone else.
pub async fn environment_detail(
    pool: &DbPool,
    id: &str,
    include_var_overrides: bool,
) -> Result<serde_json::Value, AppError> {
    let sql = format!(
        "SELECT id, name, slug, description, updated_at, {ENV_PROTECTION_SELECT},
                COALESCE(deployment_concurrency, 'off')
         FROM environments WHERE id = ?"
    );
    let row = exec::fetch_optional(pool, &sql, &[id.into()])
        .await?
        .ok_or_else(|| AppError::NotFound("environment not found".into()))?;

    let protection = protection_from_db_row(
        row.i64(5),
        &row.text(6),
        &row.text(7),
        row.i64(8),
        row.i64(9),
        row.i64(10),
    );
    let overrides = if include_var_overrides {
        list_overrides_json(pool, id).await?
    } else {
        Vec::new()
    };
    let mut obj = serde_json::json!({
        "id": row.text(0),
        "name": row.text(1),
        "slug": row.text(2),
        "description": row.text(3),
        "updated_at": row.opt_text(4),
        "overrides": overrides,
        "secret_overrides": list_secret_overrides_json(pool, id).await?,
        "deployment_concurrency": row.text(11),
        "host_allowlist": list_host_allowlist_json(pool, id).await?,
    });
    if let Some(map) = obj.as_object_mut() {
        map.extend(protection.to_api_fields());
    }
    Ok(obj)
}

pub async fn list_host_allowlist_json(
    pool: &DbPool,
    environment_id: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT kind, value, created_at FROM environment_hosts \
         WHERE environment_id = ? ORDER BY kind, value",
        &[environment_id.into()],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "kind": r.text(0),
                "value": r.text(1),
                "created_at": r.opt_text(2),
            })
        })
        .collect())
}

pub async fn environment_exists(pool: &DbPool, id: &str) -> Result<bool, AppError> {
    let count = exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM environments WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(count != 0)
}

/// Does an environment with this slug exist? (YAML `environments:` check)
pub async fn slug_exists(pool: &DbPool, slug: &str) -> Result<bool, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT EXISTS(SELECT 1 FROM environments WHERE slug = ?)",
        &[slug.into()],
    )
    .await?
        != 0)
}

/// Idempotent allowlist insert (re-adding an entry is a no-op).
pub async fn insert_host_allowlist(
    pool: &DbPool,
    environment_id: &str,
    kind: &str,
    value: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO environment_hosts (environment_id, kind, value) VALUES (?, ?, ?) \
         ON CONFLICT (environment_id, kind, value) DO NOTHING",
        &[environment_id.into(), kind.into(), value.into()],
    )
    .await?;
    Ok(())
}

/// Delete one allowlist entry; `NotFound` when it did not exist.
pub async fn delete_host_allowlist(
    pool: &DbPool,
    environment_id: &str,
    kind: &str,
    value: &str,
) -> Result<(), AppError> {
    let affected = exec::execute(
        pool,
        "DELETE FROM environment_hosts WHERE environment_id = ? AND kind = ? AND value = ?",
        &[environment_id.into(), kind.into(), value.into()],
    )
    .await?;
    if affected == 0 {
        return Err(AppError::NotFound("allowlist entry not found".into()));
    }
    Ok(())
}

pub async fn list_overrides_json(
    pool: &DbPool,
    environment_id: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT o.var_id, v.name, v.value AS default_value, o.value AS override_value, o.updated_at
         FROM environment_var_overrides o
         JOIN platform_vars v ON v.id = o.var_id
         WHERE o.environment_id = ?
         ORDER BY v.name ASC",
        &[environment_id.into()],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "var_id": r.text(0),
                "var_name": r.text(1),
                "default_value": r.text(2),
                "value": r.text(3),
                "updated_at": r.opt_text(4),
            })
        })
        .collect())
}

/// Secret overrides never expose plaintext (override or default) — `has_override` only.
pub async fn list_secret_overrides_json(
    pool: &DbPool,
    environment_id: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT o.secret_id, s.name, s.kind, o.updated_at
         FROM environment_secret_overrides o
         JOIN secrets s ON s.id = o.secret_id
         WHERE o.environment_id = ?
         ORDER BY s.name ASC",
        &[environment_id.into()],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "secret_id": r.text(0),
                "secret_name": r.text(1),
                "kind": normalize_secret_kind(Some(&r.text(2))),
                "has_override": true,
                "updated_at": r.opt_text(3),
            })
        })
        .collect())
}

/// `(name, slug)` used for `create` conflict checks and delete audit context.
pub async fn find_by_slug_or_name(
    pool: &DbPool,
    slug: &str,
    name: &str,
) -> Result<Option<(String, String)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT name, slug FROM environments WHERE slug = ? OR name = ?",
        &[slug.into(), name.into()],
    )
    .await?
    .map(|r| (r.text(0), r.text(1))))
}

/// Insert a new environment row.
pub async fn insert_environment(
    pool: &DbPool,
    id: &str,
    name: &str,
    slug: &str,
    description: &str,
    protection: &EnvironmentProtection,
    deployment_concurrency: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO environments (
            id, name, slug, description,
            protection_enabled, min_trigger_role, min_approve_role,
            require_approvals, forbid_self_approve, approval_timeout_seconds,
            deployment_concurrency
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            id.into(),
            name.into(),
            slug.into(),
            description.into(),
            SqlValue::Int(protection.enabled as i64),
            protection.min_trigger_role.as_str().into(),
            protection.min_approve_role.as_str().into(),
            SqlValue::Int(protection.require_approvals),
            SqlValue::Int(protection.forbid_self_approve as i64),
            SqlValue::Int(protection.approval_timeout_seconds),
            deployment_concurrency.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Read the settings `update` merges against.
pub struct EnvironmentSettingsRow {
    pub name: String,
    pub slug: String,
    pub description: String,
    pub protection: EnvironmentProtection,
    pub deployment_concurrency: String,
}

pub async fn settings_row(
    pool: &DbPool,
    id: &str,
) -> Result<Option<EnvironmentSettingsRow>, AppError> {
    let sql = format!(
        "SELECT name, slug, description, {ENV_PROTECTION_SELECT},
                COALESCE(deployment_concurrency, 'off')
         FROM environments WHERE id = ?"
    );
    let existing = exec::fetch_optional(pool, &sql, &[id.into()]).await?;
    Ok(existing.map(|existing| EnvironmentSettingsRow {
        name: existing.text(0),
        slug: existing.text(1),
        description: existing.text(2),
        protection: protection_from_db_row(
            existing.i64(3),
            &existing.text(4),
            &existing.text(5),
            existing.i64(6),
            existing.i64(7),
            existing.i64(8),
        ),
        deployment_concurrency: existing.text(9),
    }))
}

/// `true` when another environment already uses this name or slug.
pub async fn name_or_slug_taken(
    pool: &DbPool,
    slug: &str,
    name: &str,
    except_id: Option<&str>,
) -> Result<bool, AppError> {
    let found = match except_id {
        Some(id) => {
            exec::fetch_optional(
                pool,
                "SELECT id FROM environments WHERE (slug = ? OR name = ?) AND id != ?",
                &[slug.into(), name.into(), id.into()],
            )
            .await?
        }
        None => {
            exec::fetch_optional(
                pool,
                "SELECT id FROM environments WHERE slug = ? OR name = ?",
                &[slug.into(), name.into()],
            )
            .await?
        }
    };
    Ok(found.is_some())
}

pub async fn update_environment(
    pool: &DbPool,
    id: &str,
    name: &str,
    slug: &str,
    description: &str,
    protection: &EnvironmentProtection,
    deployment_concurrency: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE environments SET
            name = ?, slug = ?, description = ?,
            protection_enabled = ?, min_trigger_role = ?, min_approve_role = ?,
            require_approvals = ?, forbid_self_approve = ?, approval_timeout_seconds = ?,
            deployment_concurrency = ?, updated_at = datetime('now')
         WHERE id = ?",
        &[
            name.into(),
            slug.into(),
            description.into(),
            SqlValue::Int(protection.enabled as i64),
            protection.min_trigger_role.as_str().into(),
            protection.min_approve_role.as_str().into(),
            SqlValue::Int(protection.require_approvals),
            SqlValue::Int(protection.forbid_self_approve as i64),
            SqlValue::Int(protection.approval_timeout_seconds),
            deployment_concurrency.into(),
            id.into(),
        ],
    )
    .await?;
    Ok(())
}

pub async fn delete_environment(pool: &DbPool, id: &str) -> Result<(), AppError> {
    exec::execute(pool, "DELETE FROM environments WHERE id = ?", &[id.into()]).await?;
    Ok(())
}

/// Upsert one Variable override.
pub async fn upsert_override(
    pool: &DbPool,
    environment_id: &str,
    var_id: &str,
    value: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO environment_var_overrides (environment_id, var_id, value, updated_at)
         VALUES (?, ?, ?, datetime('now'))
         ON CONFLICT(environment_id, var_id) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
        &[environment_id.into(), var_id.into(), value.into()],
    )
    .await?;
    Ok(())
}

/// Delete one Variable override; `NotFound` when absent.
pub async fn delete_override(
    pool: &DbPool,
    environment_id: &str,
    var_id: &str,
) -> Result<(), AppError> {
    let affected = exec::execute(
        pool,
        "DELETE FROM environment_var_overrides WHERE environment_id = ? AND var_id = ?",
        &[environment_id.into(), var_id.into()],
    )
    .await?;
    if affected == 0 {
        return Err(AppError::NotFound("override not found".into()));
    }
    Ok(())
}

/// Upsert one Secret override. The caller encrypts: the store takes ciphertext
/// so the crypto boundary stays visible at the call site.
pub async fn upsert_secret_override(
    pool: &DbPool,
    environment_id: &str,
    secret_id: &str,
    ciphertext: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO environment_secret_overrides (environment_id, secret_id, value, updated_at)
         VALUES (?, ?, ?, datetime('now'))
         ON CONFLICT(environment_id, secret_id) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
        &[environment_id.into(), secret_id.into(), ciphertext.into()],
    )
    .await?;
    Ok(())
}

/// Delete one Secret override; `NotFound` when absent.
pub async fn delete_secret_override(
    pool: &DbPool,
    environment_id: &str,
    secret_id: &str,
) -> Result<(), AppError> {
    let affected = exec::execute(
        pool,
        "DELETE FROM environment_secret_overrides WHERE environment_id = ? AND secret_id = ?",
        &[environment_id.into(), secret_id.into()],
    )
    .await?;
    if affected == 0 {
        return Err(AppError::NotFound("secret override not found".into()));
    }
    Ok(())
}

/// `(name, kind)` of a secret, for override audit/response metadata.
pub async fn secret_name_and_kind(
    pool: &DbPool,
    secret_id: &str,
) -> Result<Option<(String, String)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT name, kind FROM secrets WHERE id = ?",
        &[secret_id.into()],
    )
    .await?
    .map(|r| (r.text(0), r.text(1))))
}

/// `name` of a platform variable, for override audit/response metadata.
pub async fn var_name(pool: &DbPool, var_id: &str) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT name FROM platform_vars WHERE id = ?",
        &[var_id.into()],
    )
    .await?
    .map(|r| r.text(0)))
}

/// Validate the slugs a pipeline declares under `environments`: non-empty and
/// all present in the environments registry (same bar as team slugs).
pub async fn validate_environment_slugs(pool: &DbPool, slugs: &[String]) -> Result<(), AppError> {
    if slugs.is_empty() {
        return Err(AppError::ValidationFailed(
            "environments is required — declare which environments this pipeline targets".into(),
        ));
    }
    let mut missing: Vec<String> = Vec::new();
    for raw_slug in slugs {
        let slug = raw_slug.trim();
        if slug.is_empty() || raw_slug != slug {
            missing.push(raw_slug.to_string());
            continue;
        }
        if !slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
            || slug.starts_with('-')
            || slug.ends_with('-')
            || slug.len() > 64
        {
            missing.push(raw_slug.to_string());
            continue;
        }
        let exists = exec::fetch_scalar_i64(
            pool,
            "SELECT COUNT(*) FROM environments WHERE slug = ?",
            &[slug.into()],
        )
        .await?;
        if exists == 0 {
            missing.push(slug.to_string());
        }
    }
    if !missing.is_empty() {
        return Err(AppError::ValidationFailed(format!(
            "unknown environment(s): {} — create them under Environments first",
            missing.join(", ")
        )));
    }
    Ok(())
}

/// Resolve environment slug/id → id + slug + protection. Empty/None → None.
pub async fn resolve_environment(
    pool: &DbPool,
    slug_or_id: Option<&str>,
) -> Result<Option<ResolvedEnvironment>, AppError> {
    let Some(raw) = slug_or_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let sql = format!(
        "SELECT id, slug, {ENV_PROTECTION_SELECT}
         FROM environments WHERE slug = ? OR id = ?"
    );
    let row = exec::fetch_optional(pool, &sql, &[raw.into(), raw.into()])
        .await?
        .ok_or_else(|| AppError::NotFound(format!("environment '{raw}' not found")))?;
    Ok(Some(ResolvedEnvironment {
        id: row.text(0),
        slug: row.text(1),
        protection: protection_from_db_row(
            row.i64(2),
            &row.text(3),
            &row.text(4),
            row.i64(5),
            row.i64(6),
            row.i64(7),
        ),
    }))
}

/// Load decrypted Environment secret overrides (name → plaintext) for claim.
pub async fn load_secret_override_plaintexts(
    pool: &DbPool,
    environment_id: &str,
    jwt_secret: &str,
) -> Result<std::collections::HashMap<String, String>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT s.name, o.value
         FROM environment_secret_overrides o
         JOIN secrets s ON s.id = o.secret_id
         WHERE o.environment_id = ?",
        &[environment_id.into()],
    )
    .await?;
    let mut map = std::collections::HashMap::new();
    for r in rows {
        let name = r.text(0);
        let cipher = r.text(1);
        match crypto::decrypt(&cipher, jwt_secret) {
            Ok(plain) => {
                map.insert(name, plain);
            }
            Err(e) => {
                tracing::warn!(
                    environment_id = %environment_id,
                    secret = %name,
                    error = %e,
                    "environments: failed to decrypt secret override — skipped"
                );
            }
        }
    }
    Ok(map)
}

/// `(name, slug)` for one environment — the audit/log context of override writes.
pub async fn name_and_slug(pool: &DbPool, id: &str) -> Result<Option<(String, String)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT name, slug FROM environments WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| (r.text(0), r.text(1))))
}
