//! Recent SSO authentication events for the SSO settings page.
//!
//! Visibility decision (2026-11-21): a platform administrator may read **all**
//! SSO auth events, not only rows where they are the actor (unlike
//! `/audit-logs`, whose non-super tier is actor-scoped). The settings page is
//! the operational console for SSO, so hiding other colleagues' sign-ins
//! there would make it useless. The scope is deliberately narrow: only the
//! OIDC event vocabulary below — local password logins (`auth.login`) stay
//! behind the stricter audit-log rules.

use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;

use crate::middleware::PlatformAdmin;
use crate::state::AppState;

/// The audited SSO lifecycle events shown on the settings page.
const SSO_EVENT_ACTIONS: &[&str] = &[
    "auth.oidc_login",
    "auth.oidc_provision",
    "auth.oidc_identity_approve",
    "auth.oidc_identity_reject",
    "auth.oidc_backchannel_logout",
];

#[derive(Deserialize)]
pub struct ListEventsQuery {
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_page_size")]
    pub page_size: u32,
}

fn default_page() -> u32 {
    1
}

fn default_page_size() -> u32 {
    20
}

/// `GET /api/v1/auth/oidc/events?page=&page_size=` — newest SSO events first
/// (page size capped at 100), with the total count for the table footer.
pub async fn list_events(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let page_size = query.page_size.clamp(1, 100);
    let page = query.page.max(1);
    let offset = (page - 1).saturating_mul(page_size);
    let (items, total) =
        crate::store::audit_logs::recent_actions(&state.pool, SSO_EVENT_ACTIONS, page_size, offset)
            .await?;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "items": items,
        "total": total,
        "page": page,
        "page_size": page_size,
    }))))
}
