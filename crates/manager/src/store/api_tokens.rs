//! API-token CRUD (the SQL behind platform-admin token management).

use sqlx::{Row, SqlitePool};

use shared::error::AppError;

/// Insert a token row. Only the SHA-256 hash is stored.
pub async fn insert(
    pool: &SqlitePool,
    id: &str,
    user_id: &str,
    name: &str,
    token_hash: &str,
    token_prefix: &str,
    scope: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix, scope)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(user_id)
    .bind(name)
    .bind(token_hash)
    .bind(token_prefix)
    .bind(scope)
    .execute(pool)
    .await?;
    Ok(())
}

/// List tokens as API payloads. `restrict_user_id = None` (super admin) sees
/// every token; otherwise only that user's.
pub async fn list(
    pool: &SqlitePool,
    restrict_user_id: Option<&str>,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = match restrict_user_id {
        None => {
            sqlx::query(
                "SELECT t.id, t.user_id, u.username, t.name, t.token_prefix,
                        t.created_at, t.last_used_at, t.revoked_at, t.scope
                 FROM api_tokens t
                 LEFT JOIN users u ON u.id = t.user_id
                 ORDER BY t.created_at DESC",
            )
            .fetch_all(pool)
            .await?
        }
        Some(user_id) => {
            sqlx::query(
                "SELECT t.id, t.user_id, u.username, t.name, t.token_prefix,
                        t.created_at, t.last_used_at, t.revoked_at, t.scope
                 FROM api_tokens t
                 LEFT JOIN users u ON u.id = t.user_id
                 WHERE t.user_id = ?
                 ORDER BY t.created_at DESC",
            )
            .bind(user_id)
            .fetch_all(pool)
            .await?
        }
    };

    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "user_id": r.get::<String, _>(1),
                "username": r.get::<String, _>(2),
                "name": r.get::<String, _>(3),
                "prefix": r.get::<String, _>(4),
                "created_at": r.get::<String, _>(5),
                "last_used_at": r.get::<Option<String>, _>(6),
                "revoked_at": r.get::<Option<String>, _>(7),
                "revoked": r.get::<Option<String>, _>(7).is_some(),
                "scope": r.get::<String, _>(8),
            })
        })
        .collect())
}

/// `(owner_user_id, name)` of an unrevoked token, for the authorize step.
pub async fn find_active(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<(String, String)>, AppError> {
    Ok(
        sqlx::query("SELECT user_id, name FROM api_tokens WHERE id = ? AND revoked_at IS NULL")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .map(|r| (r.get(0), r.get(1))),
    )
}

/// Revoke a token (idempotent where-guard).
pub async fn revoke(pool: &SqlitePool, id: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE api_tokens SET revoked_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
