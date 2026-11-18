//! External OIDC identities: the `(provider, subject) → user` binding.
//!
//! `subject` is the IdP's stable `sub` claim; email and display name are
//! display-only attributes refreshed at each sign-in and never used as lookup
//! keys (an email can change; `sub` cannot). `groups_json` mirrors the
//! IdP `groups` claim for display and audit — it carries no authorization in
//! this phase (see `docs platform/oidc-sso.md`).

use shared::error::AppError;

use crate::store::exec::{self, DbPool};

/// One bound external identity.
#[derive(Debug, Clone)]
pub struct IdentityRow {
    pub id: String,
    pub user_id: String,
    pub issuer: String,
    pub email: String,
    pub display_name: String,
    pub groups_json: String,
}

/// The identity bound to `(provider, subject)`, if any. This is the sign-in
/// lookup — the only key besides the user's own id that resolves to an
/// account.
pub async fn find_by_subject(
    pool: &DbPool,
    provider: &str,
    subject: &str,
) -> Result<Option<IdentityRow>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id, user_id, issuer, email, display_name, groups_json
         FROM external_identities
         WHERE provider = ? AND subject = ?",
        &[provider.into(), subject.into()],
    )
    .await?
    .map(|r| IdentityRow {
        id: r.text_of("id"),
        user_id: r.text_of("user_id"),
        issuer: r.text_of("issuer"),
        email: r.text_of("email"),
        display_name: r.text_of("display_name"),
        groups_json: r.text_of("groups_json"),
    }))
}

/// Bind one identity to one user. `(provider, subject)` and
/// `(provider, user_id)` are both unique — a duplicate surfaces as `Conflict`
/// (the caller maps it to a clear refusal rather than a silent rebind).
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    pool: &DbPool,
    id: &str,
    provider: &str,
    issuer: &str,
    subject: &str,
    user_id: &str,
    email: &str,
    display_name: &str,
    groups_json: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO external_identities
            (id, provider, issuer, subject, user_id, email, display_name, groups_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            id.into(),
            provider.into(),
            issuer.into(),
            subject.into(),
            user_id.into(),
            email.into(),
            display_name.into(),
            groups_json.into(),
        ],
    )
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") || e.to_string().contains("duplicate key") {
            AppError::Conflict("identity is already bound to an account".into())
        } else {
            AppError::DatabaseError(e.to_string())
        }
    })?;
    Ok(())
}

/// Refresh the display attributes and stamp the sign-in. The claims are
/// authoritative for `email`/`name` display; `sub` identity never changes.
pub async fn touch_login(
    pool: &DbPool,
    id: &str,
    email: &str,
    display_name: &str,
    groups_json: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE external_identities
         SET email = ?, display_name = ?, groups_json = ?, last_login_at = datetime('now')
         WHERE id = ?",
        &[
            email.into(),
            display_name.into(),
            groups_json.into(),
            id.into(),
        ],
    )
    .await?;
    Ok(())
}
