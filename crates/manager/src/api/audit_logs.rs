//! Platform-admin audit log listing.

use axum::{
    extract::{Query, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;

use crate::middleware::PlatformAdmin;
use crate::state::AppState;
use crate::store::audit_logs::{search, AuditLogFilter};

#[derive(Debug, serde::Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub resource_type: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
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

/// GET /audit-logs
///
/// Access tiers:
/// - Super admin: sees every audit record.
/// - Regular platform admin: only records where they are the actor
///   (`actor_user_id = own id`). Non-admins are rejected by the extractor.
pub async fn list(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let page = q.page.max(1);
    let page_size = q.page_size.clamp(1, 100);

    let filter = AuditLogFilter {
        // Tiered visibility — regular admins only see their own actions.
        restrict_actor_user_id: if auth.is_super_admin {
            None
        } else {
            Some(auth.id.clone())
        },
        action: q.action,
        actor: q.actor,
        resource_type: q.resource_type,
        q: q.q,
        from: q.from,
        to: q.to,
        limit: page_size,
        offset: (page - 1) * page_size,
    };

    let (items, total) = search(
        &crate::store::exec::DbPool::from_sqlite(&state.pool),
        &filter,
    )
    .await?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "items": items,
        "total": total,
        "page": page,
        "page_size": page_size,
    }))))
}
