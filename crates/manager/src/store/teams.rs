//! Teams and their membership: CRUD with the referential delete guard
//! (pipelines/secrets/runners must be gone first) and the member
//! upsert/remove paths with the last-owner guard.

use sqlx::{Row, SqlitePool};

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

/// Teams visible to the caller: `None` = platform admin (all rows);
/// `Some([])` = none (non-admins without teams see nothing);
/// `Some(ids)` = only those teams. Name-ordered.
pub async fn list(
    pool: &SqlitePool,
    visible_team_ids: Option<&[String]>,
) -> Result<Vec<TeamRow>, AppError> {
    let rows = match visible_team_ids {
        None => {
            sqlx::query(
            "SELECT id, name, slug, description, created_at, updated_at FROM teams ORDER BY name ASC",
            )
            .fetch_all(pool)
            .await?
        }
        Some([]) => Vec::new(),
        Some(ids) => {
            let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
            "SELECT id, name, slug, description, created_at, updated_at
             FROM teams WHERE id IN ({placeholders}) ORDER BY name ASC"
            );
            let mut q = sqlx::query(&sql);
            for id in ids {
                q = q.bind(id);
            }
            q.fetch_all(pool).await?
        }
    };
    Ok(rows
        .into_iter()
        .map(|r| TeamRow {
            id: r.get(0),
            name: r.get(1),
            slug: r.get(2),
            description: r.get(3),
            created_at: r.get(4),
            updated_at: r.get(5),
        })
        .collect())
}

/// Insert one team. A duplicate slug (UNIQUE constraint) surfaces as
/// `Conflict` — the create path's contract.
pub async fn insert(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    slug: &str,
    description: &str,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO teams (id, name, slug, description) VALUES (?, ?, ?, ?)")
        .bind(id)
        .bind(name)
        .bind(slug)
        .bind(description)
        .execute(pool)
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
pub async fn attach_owner(pool: &SqlitePool, team_id: &str, user_id: &str) -> Result<(), AppError> {
    sqlx::query(
        "INSERT OR IGNORE INTO team_members (team_id, user_id, role) VALUES (?, ?, 'owner')",
    )
    .bind(team_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// One team row by id (`None` = no such team).
pub async fn find_by_id(pool: &SqlitePool, id: &str) -> Result<Option<TeamRow>, AppError> {
    let row = sqlx::query(
        "SELECT id, name, slug, description, created_at, updated_at FROM teams WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| TeamRow {
        id: r.get(0),
        name: r.get(1),
        slug: r.get(2),
        description: r.get(3),
        created_at: r.get(4),
        updated_at: r.get(5),
    }))
}

/// `(name, slug, description)` — the audit before/after snapshot facts
/// (`None` = no such team).
#[derive(Debug, Clone)]
pub struct TeamFacts {
    pub name: String,
    pub slug: String,
    pub description: String,
}

pub async fn facts(pool: &SqlitePool, id: &str) -> Result<Option<TeamFacts>, AppError> {
    let row = sqlx::query("SELECT name, slug, description FROM teams WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| TeamFacts {
        name: r.get(0),
        slug: r.get(1),
        description: r.get(2),
    }))
}

/// Replace one team's name.
pub async fn update_name(pool: &SqlitePool, id: &str, name: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE teams SET name = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace one team's description.
pub async fn update_description(
    pool: &SqlitePool,
    id: &str,
    description: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE teams SET description = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(description)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Pipeline count bound to a team — part of the delete guard.
pub async fn pipeline_count(pool: &SqlitePool, team_id: &str) -> Result<i64, AppError> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM pipelines WHERE team_id = ?")
            .bind(team_id)
            .fetch_one(pool)
            .await?,
    )
}

/// Secret count bound to a team — part of the delete guard.
pub async fn secret_count(pool: &SqlitePool, team_id: &str) -> Result<i64, AppError> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM secrets WHERE team_id = ?")
            .bind(team_id)
            .fetch_one(pool)
            .await?,
    )
}

/// Runner count bound to a team — part of the delete guard.
pub async fn runner_count(pool: &SqlitePool, team_id: &str) -> Result<i64, AppError> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM runners WHERE team_id = ?")
            .bind(team_id)
            .fetch_one(pool)
            .await?,
    )
}

/// Delete one team; returns `false` when nothing matched (caller surfaces
/// `NotFound`).
pub async fn delete(pool: &SqlitePool, id: &str) -> Result<bool, AppError> {
    let result = sqlx::query("DELETE FROM teams WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Members of one team joined with their user records, username-ordered —
/// already shaped as the API payload.
pub async fn list_members(
    pool: &SqlitePool,
    team_id: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = sqlx::query(
        "SELECT u.id, u.username, u.display_name, tm.role
         FROM team_members tm
         JOIN users u ON u.id = tm.user_id
         WHERE tm.team_id = ?
         ORDER BY u.username ASC",
    )
    .bind(team_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "user_id": r.get::<String, _>(0),
                "username": r.get::<String, _>(1),
                "display_name": r.get::<String, _>(2),
                "role": r.get::<String, _>(3),
            })
        })
        .collect())
}

/// Id of an active user (`None` = no such user or deactivated) — the
/// member-upsert existence check.
pub async fn active_user_id(pool: &SqlitePool, user_id: &str) -> Result<Option<String>, AppError> {
    Ok(
        sqlx::query_scalar("SELECT id FROM users WHERE id = ? AND is_active = 1")
            .bind(user_id)
            .fetch_optional(pool)
            .await?,
    )
}

/// A membership's current role (`None` = not a member) — the audit
/// before-snapshot and the last-owner guard read this.
pub async fn member_role(
    pool: &SqlitePool,
    team_id: &str,
    user_id: &str,
) -> Result<Option<String>, AppError> {
    Ok(
        sqlx::query_scalar("SELECT role FROM team_members WHERE team_id = ? AND user_id = ?")
            .bind(team_id)
            .bind(user_id)
            .fetch_optional(pool)
            .await?,
    )
}

/// Insert or update one membership's role.
pub async fn upsert_member(
    pool: &SqlitePool,
    team_id: &str,
    user_id: &str,
    role: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO team_members (team_id, user_id, role) VALUES (?, ?, ?)
         ON CONFLICT(team_id, user_id) DO UPDATE SET role = excluded.role",
    )
    .bind(team_id)
    .bind(user_id)
    .bind(role)
    .execute(pool)
    .await?;
    Ok(())
}

/// Owner count of a team — the "cannot remove the last team owner" guard.
pub async fn owner_count(pool: &SqlitePool, team_id: &str) -> Result<i64, AppError> {
    Ok(
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM team_members WHERE team_id = ? AND role = 'owner'",
        )
        .bind(team_id)
        .fetch_one(pool)
        .await?,
    )
}

/// Remove one membership; returns `false` when nothing matched (caller
/// surfaces `NotFound`).
pub async fn remove_member(
    pool: &SqlitePool,
    team_id: &str,
    user_id: &str,
) -> Result<bool, AppError> {
    let result = sqlx::query("DELETE FROM team_members WHERE team_id = ? AND user_id = ?")
        .bind(team_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
