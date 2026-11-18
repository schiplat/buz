//! SSO browser round trip: authorization flows and session-handoff tickets.
//!
//! Both tables hold only **hashes** of the browser-facing secrets — the
//! plaintext `state` and ticket exist solely inside the response to the
//! browser and are never persisted. Rows are single-use: consumption is an
//! atomic guarded `UPDATE … WHERE consumed_at IS NULL AND unexpired
//! RETURNING`, so a replayed state or ticket finds nothing. Expired and
//! consumed rows are swept on every `start` (tiny tables, no background job
//! needed).

use shared::error::AppError;

use crate::store::exec::{self, DbPool};

pub const FLOW_TTL_SECONDS: i64 = 600;
pub const TICKET_TTL_SECONDS: i64 = 60;

/// Record a new authorization flow. `state_hash` is
/// `sha256(state)` hex; the plaintext state is never stored.
#[allow(clippy::too_many_arguments)]
pub async fn insert_flow(
    pool: &DbPool,
    id: &str,
    state_hash: &str,
    nonce: &str,
    code_verifier: &str,
    redirect_to: &str,
    expires_at: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO sso_flows
            (id, state_hash, nonce, code_verifier, redirect_to, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
        &[
            id.into(),
            state_hash.into(),
            nonce.into(),
            code_verifier.into(),
            redirect_to.into(),
            expires_at.into(),
        ],
    )
    .await?;
    Ok(())
}

/// The stored facts of one flow at consumption time.
pub struct ConsumedFlow {
    pub id: String,
    pub nonce: String,
    pub code_verifier: String,
    pub redirect_to: String,
}

/// Atomically consume the flow for `state_hash`: a replayed, expired, or
/// already-consumed state finds no row (`None`) and the caller refuses the
/// sign-in.
pub async fn consume_flow(
    pool: &DbPool,
    state_hash: &str,
) -> Result<Option<ConsumedFlow>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "UPDATE sso_flows
         SET consumed_at = datetime('now')
         WHERE state_hash = ?
           AND consumed_at IS NULL
           AND expires_at > datetime('now')
         RETURNING id, nonce, code_verifier, redirect_to",
        &[state_hash.into()],
    )
    .await?
    .map(|r| ConsumedFlow {
        id: r.text_of("id"),
        nonce: r.text_of("nonce"),
        code_verifier: r.text_of("code_verifier"),
        redirect_to: r.text_of("redirect_to"),
    }))
}

/// Record a session-handoff ticket for `user_id`. `ticket_hash` is
/// `sha256(ticket)` hex.
pub async fn insert_ticket(
    pool: &DbPool,
    ticket_hash: &str,
    user_id: &str,
    expires_at: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO sso_tickets (ticket_hash, user_id, expires_at) VALUES (?, ?, ?)",
        &[ticket_hash.into(), user_id.into(), expires_at.into()],
    )
    .await?;
    Ok(())
}

/// Atomically consume the ticket: `Some(user_id)` exactly once, within its
/// TTL; every later presentation returns `None`.
pub async fn consume_ticket(pool: &DbPool, ticket_hash: &str) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "UPDATE sso_tickets
         SET consumed_at = datetime('now')
         WHERE ticket_hash = ?
           AND consumed_at IS NULL
           AND expires_at > datetime('now')
         RETURNING user_id",
        &[ticket_hash.into()],
    )
    .await?
    .map(|r| r.text_of("user_id")))
}

/// Sweep consumed and expired rows. Runs inline on every `/oidc/start` —
/// the tables are one row per browser flow, so this stays a no-op in
/// practice while guaranteeing nothing accumulates.
pub async fn sweep(pool: &DbPool) -> Result<(), AppError> {
    exec::execute(
        pool,
        "DELETE FROM sso_flows WHERE consumed_at IS NOT NULL OR expires_at < datetime('now')",
        &[],
    )
    .await?;
    exec::execute(
        pool,
        "DELETE FROM sso_tickets WHERE consumed_at IS NOT NULL OR expires_at < datetime('now')",
        &[],
    )
    .await?;
    Ok(())
}

/// `expires_at` text for a row created now: `now + ttl` in the same
/// `YYYY-MM-DD HH24:MI:SS` format every timestamp comparison expects.
pub fn expires_at_from_now(ttl_seconds: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(ttl_seconds))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}
