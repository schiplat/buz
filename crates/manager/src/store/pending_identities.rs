//! Identities refused at sign-in because `auto_provision` is off
//! (`platform/oidc-sso.md`).
//!
//! The sign-in callback records (or refreshes) one row per
//! `(provider, subject)`; the administrator queue lists them and either
//! approves — bind to an existing user, or create a plain SSO user — or
//! dismisses them. Recording is display/queue data only: it never influences
//! sign-in decisions, and it carries no authorization.

use shared::error::AppError;

use crate::store::exec::{self, DbPool};

/// One unresolved identity waiting for an administrator.
#[derive(Debug, Clone)]
pub struct PendingIdentityRow {
    pub id: String,
    pub provider: String,
    pub issuer: String,
    pub subject: String,
    pub email: String,
    pub display_name: String,
    pub groups_json: String,
    pub first_seen_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub attempts: i64,
}

fn row_of(row: &crate::store::exec::GenericRow) -> PendingIdentityRow {
    PendingIdentityRow {
        id: row.text_of("id"),
        provider: row.text_of("provider"),
        issuer: row.text_of("issuer"),
        subject: row.text_of("subject"),
        email: row.text_of("email"),
        display_name: row.text_of("display_name"),
        groups_json: row.text_of("groups_json"),
        first_seen_at: row.opt_text_of("first_seen_at"),
        last_seen_at: row.opt_text_of("last_seen_at"),
        attempts: row.i64_of("attempts"),
    }
}

/// Record (or refresh) a refused identity. Claims are authoritative for the
/// display attributes; `attempts` counts refused sign-ins and `last_seen_at`
/// moves forward — an existing row is never reset to an earlier state.
#[allow(clippy::too_many_arguments)]
pub async fn record_refused(
    pool: &DbPool,
    id: &str,
    provider: &str,
    issuer: &str,
    subject: &str,
    email: &str,
    display_name: &str,
    groups_json: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO pending_identities
            (id, provider, issuer, subject, email, display_name, groups_json)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (provider, subject) DO UPDATE SET
            email = excluded.email,
            display_name = excluded.display_name,
            groups_json = excluded.groups_json,
            last_seen_at = datetime('now'),
            attempts = pending_identities.attempts + 1",
        &[
            id.into(),
            provider.into(),
            issuer.into(),
            subject.into(),
            email.into(),
            display_name.into(),
            groups_json.into(),
        ],
    )
    .await?;
    Ok(())
}

/// The administrator queue, oldest first attempt at the top.
pub async fn list(pool: &DbPool) -> Result<Vec<PendingIdentityRow>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT id, provider, issuer, subject, email, display_name, groups_json,
                first_seen_at, last_seen_at, attempts
         FROM pending_identities
         ORDER BY first_seen_at ASC, subject ASC",
        &[],
    )
    .await?;
    Ok(rows.iter().map(row_of).collect())
}

/// One pending row inside the caller's transaction (approval/rejection).
pub async fn get_tx(
    tx: &mut exec::DbTx<'_>,
    id: &str,
) -> Result<Option<PendingIdentityRow>, AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT id, provider, issuer, subject, email, display_name, groups_json,
                    first_seen_at, last_seen_at, attempts
             FROM pending_identities WHERE id = ?",
            &[id.into()],
        )
        .await?
        .map(|row| row_of(&row)))
}

/// Remove the pending row inside the caller's transaction (consumed by
/// approval or dismissal).
pub async fn delete_tx(tx: &mut exec::DbTx<'_>, id: &str) -> Result<(), AppError> {
    tx.execute("DELETE FROM pending_identities WHERE id = ?", &[id.into()])
        .await?;
    Ok(())
}
