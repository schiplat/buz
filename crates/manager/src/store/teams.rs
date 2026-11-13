//! Teams and their membership: CRUD with the referential delete guard
//! (pipelines/secrets/runners must be gone first) and the member
//! upsert/remove paths with the last-owner guard.
//!
//! On the dual-backend dispatcher (`store::exec`): statements stay
//! SQLite-dialect strings with backend-neutral binds; rows decode by name.

use crate::store::exec::{self, DbPool, SqlValue};

use shared::error::AppError;

/// One team row, list/detail shape.
#[derive(Debug, Clone)]
pub struct TeamRow {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

fn team_row(r: exec::GenericRow) -> TeamRow {
    TeamRow {
        id: r.text_of("id"),
        name: r.text_of("name"),
        slug: r.text_of("slug"),
        description: r.text_of("description"),
        created_at: r.opt_text_of("created_at"),
        updated_at: r.opt_text_of("updated_at"),
    }
}

/// Teams visible to the caller: `None` = platform admin (all rows);
/// `Some([])` = none (non-admins without teams see nothing);
/// `Some(ids)` = only those teams. Name-ordered.
pub async fn list(
    pool: &DbPool,
    visible_team_ids: Option<&[String]>,
) -> Result<Vec<TeamRow>, AppError> {
    let rows = match visible_team_ids {
        None => {
            exec::fetch_all(
                pool,
                "SELECT id, name, slug, description, created_at, updated_at FROM teams ORDER BY name ASC",
                &[],
            )
            .await?
        }
        Some([]) => Vec::new(),
        Some(ids) => {
            let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT id, name, slug, description, created_at, updated_at
             FROM teams WHERE id IN ({placeholders}) ORDER BY name ASC"
            );
            let binds: Vec<SqlValue> = ids.iter().map(|id| id.as_str().into()).collect();
            exec::fetch_all(pool, &sql, &binds).await?
        }
    };
    Ok(rows.into_iter().map(team_row).collect())
}

/// Insert one team. A duplicate slug (UNIQUE constraint) surfaces as
/// `Conflict` — the create path's contract.
pub async fn insert(
    pool: &DbPool,
    id: &str,
    name: &str,
    slug: &str,
    description: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO teams (id, name, slug, description) VALUES (?, ?, ?, ?)",
        &[id.into(), name.into(), slug.into(), description.into()],
    )
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::Conflict("team slug already exists".into())
        } else {
            AppError::DatabaseError(e.to_string())
        }
    })?;
    Ok(())
}

/// Attach a user to a team as owner (no-op when already a member) — the
/// creator-visibility step of team creation.
pub async fn attach_owner(pool: &DbPool, team_id: &str, user_id: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT OR IGNORE INTO team_members (team_id, user_id, role) VALUES (?, ?, 'owner')",
        &[team_id.into(), user_id.into()],
    )
    .await?;
    Ok(())
}

/// One team row by id (`None` = no such team).
pub async fn find_by_id(pool: &DbPool, id: &str) -> Result<Option<TeamRow>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id, name, slug, description, created_at, updated_at FROM teams WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(team_row))
}

/// `(name, slug, description)` — the audit before/after snapshot facts
/// (`None` = no such team).
#[derive(Debug, Clone)]
pub struct TeamFacts {
    pub name: String,
    pub slug: String,
    pub description: String,
}

pub async fn facts(pool: &DbPool, id: &str) -> Result<Option<TeamFacts>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT name, slug, description FROM teams WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| TeamFacts {
        name: r.text_of("name"),
        slug: r.text_of("slug"),
        description: r.text_of("description"),
    }))
}

/// Replace one team's name.
pub async fn update_name(pool: &DbPool, id: &str, name: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE teams SET name = ?, updated_at = datetime('now') WHERE id = ?",
        &[name.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace one team's description.
pub async fn update_description(
    pool: &DbPool,
    id: &str,
    description: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE teams SET description = ?, updated_at = datetime('now') WHERE id = ?",
        &[description.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Pipeline count bound to a team — part of the delete guard.
pub async fn pipeline_count(pool: &DbPool, team_id: &str) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM pipelines WHERE team_id = ?",
        &[team_id.into()],
    )
    .await?)
}

/// Secret count bound to a team — part of the delete guard.
pub async fn secret_count(pool: &DbPool, team_id: &str) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM secrets WHERE team_id = ?",
        &[team_id.into()],
    )
    .await?)
}

/// Runner count bound to a team — part of the delete guard.
pub async fn runner_count(pool: &DbPool, team_id: &str) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM runners WHERE team_id = ?",
        &[team_id.into()],
    )
    .await?)
}

/// Delete one team; returns `false` when nothing matched (caller surfaces
/// `NotFound`).
pub async fn delete(pool: &DbPool, id: &str) -> Result<bool, AppError> {
    let affected = exec::execute(pool, "DELETE FROM teams WHERE id = ?", &[id.into()]).await?;
    Ok(affected > 0)
}

/// Members of one team joined with their user records, username-ordered —
/// already shaped as the API payload.
pub async fn list_members(
    pool: &DbPool,
    team_id: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT u.id, u.username, u.display_name, tm.role
         FROM team_members tm
         JOIN users u ON u.id = tm.user_id
         WHERE tm.team_id = ?
         ORDER BY u.username ASC",
        &[team_id.into()],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "user_id": r.text_of("id"),
                "username": r.text_of("username"),
                "display_name": r.text_of("display_name"),
                "role": r.text_of("role"),
            })
        })
        .collect())
}

/// Id of an active user (`None` = no such user or deactivated) — the
/// member-upsert existence check.
pub async fn active_user_id(pool: &DbPool, user_id: &str) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id FROM users WHERE id = ? AND is_active = 1",
        &[user_id.into()],
    )
    .await?
    .map(|r| r.text_of("id")))
}

/// A membership's current role (`None` = not a member) — the audit
/// before-snapshot and the last-owner guard read this.
pub async fn member_role(
    pool: &DbPool,
    team_id: &str,
    user_id: &str,
) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT role FROM team_members WHERE team_id = ? AND user_id = ?",
        &[team_id.into(), user_id.into()],
    )
    .await?
    .map(|r| r.text_of("role")))
}

/// Insert or update one membership's role.
pub async fn upsert_member(
    pool: &DbPool,
    team_id: &str,
    user_id: &str,
    role: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO team_members (team_id, user_id, role) VALUES (?, ?, ?)
         ON CONFLICT(team_id, user_id) DO UPDATE SET role = excluded.role",
        &[team_id.into(), user_id.into(), role.into()],
    )
    .await?;
    Ok(())
}

/// Owner count of a team — the "cannot remove the last team owner" guard.
pub async fn owner_count(pool: &DbPool, team_id: &str) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM team_members WHERE team_id = ? AND role = 'owner'",
        &[team_id.into()],
    )
    .await?)
}

/// Remove one membership; returns `false` when nothing matched (caller
/// surfaces `NotFound`).
pub async fn remove_member(pool: &DbPool, team_id: &str, user_id: &str) -> Result<bool, AppError> {
    let affected = exec::execute(
        pool,
        "DELETE FROM team_members WHERE team_id = ? AND user_id = ?",
        &[team_id.into(), user_id.into()],
    )
    .await?;
    Ok(affected > 0)
}

/// `(id, slug)` by id (`None` = no such team) — request team resolution.
pub async fn id_and_slug_by_id(
    pool: &DbPool,
    id: &str,
) -> Result<Option<(String, String)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id, slug FROM teams WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| (r.text(0), r.text(1))))
}

/// Startup team bootstrap: create the `default` team, attach active platform
/// admins as owners, create teams for legacy `owner_group` values, and
/// backfill `team_id` on pipelines/secrets. Idempotent; failures that the
/// legacy path swallowed stay best effort.
pub async fn bootstrap(pool: &DbPool) -> Result<(), AppError> {
    let default_team_id = ensure(pool, "default", "Default").await?;

    for admin_id in crate::store::users::active_platform_admin_ids(pool).await? {
        exec::execute(
            pool,
            "INSERT OR IGNORE INTO team_members (team_id, user_id, role) VALUES (?, ?, 'owner')",
            &[default_team_id.as_str().into(), admin_id.as_str().into()],
        )
        .await?;
    }

    let groups = exec::fetch_all(
        pool,
        "SELECT DISTINCT owner_group FROM (
            SELECT owner_group FROM pipelines WHERE owner_group != ''
            UNION
            SELECT owner_group FROM secrets WHERE owner_group != ''
         ) AS legacy_groups",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| r.text(0))
    .collect::<Vec<_>>();

    for slug in groups {
        ensure(pool, &slug, &slug).await?;
    }

    // Match by owner_group slug, then send the remainder to the default team.
    let _ = exec::execute(
        pool,
        "UPDATE pipelines SET team_id = (
            SELECT t.id FROM teams t WHERE t.slug = pipelines.owner_group
         )
         WHERE (team_id IS NULL OR team_id = '')
           AND owner_group != ''
           AND EXISTS (SELECT 1 FROM teams t WHERE t.slug = pipelines.owner_group)",
        &[],
    )
    .await;
    let _ = exec::execute(
        pool,
        "UPDATE secrets SET team_id = (
            SELECT t.id FROM teams t WHERE t.slug = secrets.owner_group
         )
         WHERE (team_id IS NULL OR team_id = '')
           AND owner_group != ''
           AND EXISTS (SELECT 1 FROM teams t WHERE t.slug = secrets.owner_group)",
        &[],
    )
    .await;

    let pipelines = exec::execute(
        pool,
        "UPDATE pipelines SET team_id = ?, owner_group = 'default'
         WHERE team_id IS NULL OR team_id = ''",
        &[default_team_id.as_str().into()],
    )
    .await?;
    if pipelines > 0 {
        tracing::info!("Backfilled {pipelines} rows in pipelines to default team");
    }
    let secrets = exec::execute(
        pool,
        "UPDATE secrets SET team_id = ?, owner_group = 'default'
         WHERE team_id IS NULL OR team_id = ''",
        &[default_team_id.as_str().into()],
    )
    .await?;
    if secrets > 0 {
        tracing::info!("Backfilled {secrets} rows in secrets to default team");
    }
    Ok(())
}

async fn ensure(pool: &DbPool, slug: &str, name: &str) -> Result<String, AppError> {
    if let Some(id) =
        exec::fetch_optional(pool, "SELECT id FROM teams WHERE slug = ?", &[slug.into()])
            .await?
            .map(|r| r.text(0))
    {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    exec::execute(
        pool,
        "INSERT INTO teams (id, name, slug, description) VALUES (?, ?, ?, ?)",
        &[id.as_str().into(), name.into(), slug.into(), "".into()],
    )
    .await?;
    tracing::info!("Created team '{name}' (slug={slug})");
    Ok(id)
}

/// A team's display name (`None` = no such team).
pub async fn name_of(pool: &DbPool, id: &str) -> Result<Option<String>, AppError> {
    Ok(
        exec::fetch_optional(pool, "SELECT name FROM teams WHERE id = ?", &[id.into()])
            .await?
            .map(|r| r.text_of("name")),
    )
}

/// Team id by slug (`None` = no such team) — the YAML `team:` resolution.
pub async fn id_by_slug(pool: &DbPool, slug: &str) -> Result<Option<String>, AppError> {
    Ok(
        exec::fetch_optional(pool, "SELECT id FROM teams WHERE slug = ?", &[slug.into()])
            .await?
            .map(|r| r.text(0)),
    )
}

/// `(id, slug)` by slug (`None` = no such team) — parse-and-resolve callers.
pub async fn id_and_slug_by_slug(
    pool: &DbPool,
    slug: &str,
) -> Result<Option<(String, String)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id, slug FROM teams WHERE slug = ?",
        &[slug.into()],
    )
    .await?
    .map(|r| (r.text(0), r.text(1))))
}
