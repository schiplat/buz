//! API-token CRUD (the SQL behind platform-admin token management).
//!
//! On the dual-backend dispatcher (`store::exec`).

use crate::store::exec::{self, DbPool};

use shared::error::AppError;

/// Insert a token row. Only the SHA-256 hash is stored.
pub async fn insert(
    pool: &DbPool,
    id: &str,
    user_id: &str,
    name: &str,
    token_hash: &str,
    token_prefix: &str,
    scope: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix, scope)
         VALUES (?, ?, ?, ?, ?, ?)",
        &[
            id.into(),
            user_id.into(),
            name.into(),
            token_hash.into(),
            token_prefix.into(),
            scope.into(),
        ],
    )
    .await?;
    Ok(())
}

/// List tokens as API payloads. `restrict_user_id = None` (super admin) sees
/// every token; otherwise only that user's.
pub async fn list(
    pool: &DbPool,
    restrict_user_id: Option<&str>,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = match restrict_user_id {
        None => {
            exec::fetch_all(
                pool,
                "SELECT t.id, t.user_id, u.username, t.name, t.token_prefix,
                        t.created_at, t.last_used_at, t.revoked_at, t.scope
                 FROM api_tokens t
                 LEFT JOIN users u ON u.id = t.user_id
                 ORDER BY t.created_at DESC",
                &[],
            )
            .await?
        }
        Some(user_id) => {
            exec::fetch_all(
                pool,
                "SELECT t.id, t.user_id, u.username, t.name, t.token_prefix,
                        t.created_at, t.last_used_at, t.revoked_at, t.scope
                 FROM api_tokens t
                 LEFT JOIN users u ON u.id = t.user_id
                 WHERE t.user_id = ?
                 ORDER BY t.created_at DESC",
                &[user_id.into()],
            )
            .await?
        }
    };

    Ok(rows
        .into_iter()
        .map(|r| {
            let revoked_at = r.opt_text_of("revoked_at");
            let revoked = revoked_at.is_some();
            serde_json::json!({
                "id": r.text_of("id"),
                "user_id": r.text_of("user_id"),
                "username": r.text_of("username"),
                "name": r.text_of("name"),
                "prefix": r.text_of("token_prefix"),
                "created_at": r.text_of("created_at"),
                "last_used_at": r.opt_text_of("last_used_at"),
                "revoked_at": revoked_at,
                "revoked": revoked,
                "scope": r.text_of("scope"),
            })
        })
        .collect())
}

/// `(user_id, scope)` of an unrevoked token by hash — the auth lookup.
pub async fn find_by_hash(
    pool: &DbPool,
    token_hash: &str,
) -> Result<Option<(String, String)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT user_id, scope FROM api_tokens WHERE token_hash = ? AND revoked_at IS NULL",
        &[token_hash.into()],
    )
    .await?
    .map(|r| (r.text(0), r.text(1))))
}

/// Best-effort `last_used_at` stamp (never fails the request).
pub async fn touch_last_used(pool: &DbPool, token_hash: &str) {
    let _ = exec::execute(
        pool,
        "UPDATE api_tokens SET last_used_at = datetime('now') WHERE token_hash = ?",
        &[token_hash.into()],
    )
    .await;
}

/// `(owner_user_id, name)` of an unrevoked token, for the authorize step.
pub async fn find_active(pool: &DbPool, id: &str) -> Result<Option<(String, String)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT user_id, name FROM api_tokens WHERE id = ? AND revoked_at IS NULL",
        &[id.into()],
    )
    .await?
    .map(|r| (r.text_of("user_id"), r.text_of("name"))))
}

/// Revoke a token (idempotent where-guard).
pub async fn revoke(pool: &DbPool, id: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE api_tokens SET revoked_at = datetime('now') WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(())
}
