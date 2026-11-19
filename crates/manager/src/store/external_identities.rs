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
        INSERT_SQL,
        &IdentityFacts {
            id,
            provider,
            issuer,
            subject,
            user_id,
            email,
            display_name,
            groups_json,
        }
        .bindings(),
    )
    .await
    .map_err(map_insert_error)?;
    Ok(())
}

/// [`insert`] inside the caller's transaction (identity approval).
#[allow(clippy::too_many_arguments)]
pub async fn insert_tx(
    tx: &mut crate::store::exec::DbTx<'_>,
    id: &str,
    provider: &str,
    issuer: &str,
    subject: &str,
    user_id: &str,
    email: &str,
    display_name: &str,
    groups_json: &str,
) -> Result<(), AppError> {
    tx.execute(
        INSERT_SQL,
        &IdentityFacts {
            id,
            provider,
            issuer,
            subject,
            user_id,
            email,
            display_name,
            groups_json,
        }
        .bindings(),
    )
    .await
    .map_err(map_insert_error)?;
    Ok(())
}

const INSERT_SQL: &str = "INSERT INTO external_identities
            (id, provider, issuer, subject, user_id, email, display_name, groups_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)";

/// One identity's display/lookup facts, shared by both insert paths.
struct IdentityFacts<'a> {
    id: &'a str,
    provider: &'a str,
    issuer: &'a str,
    subject: &'a str,
    user_id: &'a str,
    email: &'a str,
    display_name: &'a str,
    groups_json: &'a str,
}

impl<'a> IdentityFacts<'a> {
    fn bindings(&self) -> [crate::store::exec::SqlValue; 8] {
        [
            self.id.into(),
            self.provider.into(),
            self.issuer.into(),
            self.subject.into(),
            self.user_id.into(),
            self.email.into(),
            self.display_name.into(),
            self.groups_json.into(),
        ]
    }
}

fn map_insert_error(error: sqlx::Error) -> AppError {
    let message = error.to_string();
    if message.contains("UNIQUE") || message.contains("duplicate key") {
        AppError::Conflict("identity is already bound to an account".into())
    } else {
        AppError::DatabaseError(message)
    }
}

/// Whether `user_id` already carries any identity (used before binding, so
/// the caller can refuse with a clear message instead of a UNIQUE error).
pub async fn find_by_user(
    tx: &mut crate::store::exec::DbTx<'_>,
    provider: &str,
    user_id: &str,
) -> Result<Option<String>, AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT subject FROM external_identities WHERE provider = ? AND user_id = ?",
            &[provider.into(), user_id.into()],
        )
        .await?
        .map(|row| row.text_of("subject")))
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
