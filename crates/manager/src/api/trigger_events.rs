//! Unified pipeline trigger event log (git / webhook / manual / cron).

use axum::{
    extract::{Path, Query, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::rbac::Perm;

use crate::rbac::{self, AuthUser};
use crate::state::AppState;
use crate::store::trigger_events::{self, ListEventsFilter};

#[derive(Debug, serde::Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub decision: Option<String>,
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

/// GET /pipelines/:id/trigger-events
pub async fn list(
    auth: AuthUser,
    Path(pipeline_id): Path<String>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = rbac::pipeline_team_id(&state.pool, &pipeline_id).await?;
    auth.check_team(&team_id, Perm::TeamView)?;

    let page = q.page.max(1);
    let page_size = q.page_size.clamp(1, 100);
    let offset = (page - 1) * page_size;

    let (total, items) = trigger_events::list_events(
        &state.db(),
        &pipeline_id,
        ListEventsFilter {
            source: q.source.filter(|s| !s.is_empty()),
            decision: q.decision.filter(|d| !d.is_empty()),
        },
        auth.is_platform_admin,
        page_size as i64,
        offset as i64,
    )
    .await?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "items": items,
        "total": total,
        "page": page,
        "page_size": page_size,
    }))))
}
