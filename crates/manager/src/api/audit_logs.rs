//! Platform-admin audit log listing.

use axum::{
    extract::{Query, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use sqlx::Row;

use crate::middleware::PlatformAdmin;
use crate::state::AppState;

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
pub async fn list(
    _admin: PlatformAdmin,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let page = q.page.max(1);
    let page_size = q.page_size.clamp(1, 100);
    let offset = (page - 1) * page_size;

    let mut where_parts: Vec<&str> = Vec::new();
    let mut binds: Vec<String> = Vec::new();

    if let Some(a) = q.action.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        where_parts.push("action = ?");
        binds.push(a.to_string());
    }
    if let Some(a) = q.actor.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        where_parts.push("actor_username LIKE ?");
        binds.push(format!("%{a}%"));
    }
    if let Some(rt) = q
        .resource_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        where_parts.push("resource_type = ?");
        binds.push(rt.to_string());
    }
    if let Some(from) = q.from.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        where_parts.push("created_at >= ?");
        binds.push(from.to_string());
    }
    if let Some(to) = q.to.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        where_parts.push("created_at <= ?");
        binds.push(to.to_string());
    }
    if let Some(search) = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        where_parts.push(
            "(resource_name LIKE ? OR resource_id LIKE ? OR action LIKE ? OR actor_username LIKE ?)",
        );
        let pat = format!("%{search}%");
        binds.push(pat.clone());
        binds.push(pat.clone());
        binds.push(pat.clone());
        binds.push(pat);
    }

    let where_sql = if where_parts.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_parts.join(" AND "))
    };

    let count_sql = format!("SELECT COUNT(*) FROM audit_logs {where_sql}");
    let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql);
    for b in &binds {
        count_q = count_q.bind(b);
    }
    let total = count_q.fetch_one(&state.pool).await?;

    let list_sql = format!(
        "SELECT id, created_at, actor_user_id, actor_username, action,
                resource_type, resource_id, resource_name, team_id,
                before_json, after_json, meta_json, ip
         FROM audit_logs {where_sql}
         ORDER BY created_at DESC
         LIMIT ? OFFSET ?"
    );
    let mut list_q = sqlx::query(&list_sql);
    for b in &binds {
        list_q = list_q.bind(b);
    }
    list_q = list_q.bind(page_size as i64).bind(offset as i64);
    let rows = list_q.fetch_all(&state.pool).await?;

    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let before_raw: Option<String> = r.get(9);
            let after_raw: Option<String> = r.get(10);
            let meta_raw: Option<String> = r.get(11);
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "created_at": r.get::<String, _>(1),
                "actor_user_id": r.get::<String, _>(2),
                "actor_username": r.get::<String, _>(3),
                "action": r.get::<String, _>(4),
                "resource_type": r.get::<String, _>(5),
                "resource_id": r.get::<String, _>(6),
                "resource_name": r.get::<String, _>(7),
                "team_id": r.get::<Option<String>, _>(8),
                "before": parse_json_opt(before_raw.as_deref()),
                "after": parse_json_opt(after_raw.as_deref()),
                "meta": parse_json_opt(meta_raw.as_deref()),
                "ip": r.get::<String, _>(12),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(serde_json::json!({
        "items": items,
        "total": total,
        "page": page,
        "page_size": page_size,
    }))))
}

fn parse_json_opt(raw: Option<&str>) -> serde_json::Value {
    match raw {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.to_string()))
        }
        _ => serde_json::Value::Null,
    }
}
