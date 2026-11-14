//! Audit log search.
//!
//! On the dual-backend dispatcher (`store::exec`): the dynamic WHERE is built
//! from fixed fragments (never raw filter text) and binds travel as
//! backend-neutral `SqlValue`s; rows decode by column name.

use crate::store::exec::{self, DbPool, SqlValue};

use shared::error::AppError;

/// Search filters. `restrict_actor_user_id` encodes the visibility tier:
/// regular platform admins only see their own actions (None = unrestricted,
/// super admin).
pub struct AuditLogFilter {
    pub restrict_actor_user_id: Option<String>,
    pub action: Option<String>,
    pub actor: Option<String>,
    pub resource_type: Option<String>,
    pub q: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

/// Paged audit-log search. Returns `(items, total)` with each row already
/// shaped as the API payload (before/after/meta parsed from JSON).
pub async fn search(
    pool: &DbPool,
    filter: &AuditLogFilter,
) -> Result<(Vec<serde_json::Value>, i64), AppError> {
    let mut where_parts: Vec<String> = Vec::new();
    let mut binds: Vec<SqlValue> = Vec::new();

    if let Some(actor_id) = filter
        .restrict_actor_user_id
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        where_parts.push("actor_user_id = ?".to_string());
        binds.push(actor_id.into());
    }
    if let Some(a) = filter
        .action
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        where_parts.push("action = ?".to_string());
        binds.push(a.into());
    }
    if let Some(a) = filter
        .actor
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        where_parts.push("actor_username LIKE ?".to_string());
        binds.push(format!("%{a}%").into());
    }
    if let Some(rt) = filter
        .resource_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        where_parts.push("resource_type = ?".to_string());
        binds.push(rt.into());
    }
    if let Some(from) = filter
        .from
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        where_parts.push("created_at >= ?".to_string());
        binds.push(from.into());
    }
    if let Some(to) = filter
        .to
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        where_parts.push("created_at <= ?".to_string());
        binds.push(to.into());
    }
    if let Some(search) = filter.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        where_parts.push(
            "(resource_name LIKE ? OR resource_id LIKE ? OR action LIKE ? OR actor_username LIKE ?)"
                .to_string(),
        );
        let pat = format!("%{search}%");
        binds.push(pat.clone().into());
        binds.push(pat.clone().into());
        binds.push(pat.clone().into());
        binds.push(pat.into());
    }

    let where_sql = if where_parts.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_parts.join(" AND "))
    };

    let count_sql = format!("SELECT COUNT(*) FROM audit_logs {where_sql}");
    let total = exec::fetch_scalar_i64(pool, &count_sql, &binds).await?;

    let list_sql = format!(
        "SELECT id, created_at, actor_user_id, actor_username, action,
                resource_type, resource_id, resource_name, team_id,
                before_json, after_json, meta_json, ip
         FROM audit_logs {where_sql}
         ORDER BY created_at DESC
         LIMIT ? OFFSET ?"
    );
    binds.push(SqlValue::Int(i64::from(filter.limit)));
    binds.push(SqlValue::Int(i64::from(filter.offset)));
    let rows = exec::fetch_all(pool, &list_sql, &binds).await?;

    let items: Vec<serde_json::Value> = rows.into_iter().map(audit_row_json).collect();

    Ok((items, total))
}

fn audit_row_json(r: crate::store::exec::GenericRow) -> serde_json::Value {
    serde_json::json!({
        "id": r.text_of("id"),
        "created_at": r.text_of("created_at"),
        "actor_user_id": r.text_of("actor_user_id"),
        "actor_username": r.text_of("actor_username"),
        "action": r.text_of("action"),
        "resource_type": r.text_of("resource_type"),
        "resource_id": r.text_of("resource_id"),
        "resource_name": r.text_of("resource_name"),
        "team_id": r.opt_text_of("team_id"),
        "before": parse_json_opt(r.opt_text_of("before_json").as_deref()),
        "after": parse_json_opt(r.opt_text_of("after_json").as_deref()),
        "meta": parse_json_opt(r.opt_text_of("meta_json").as_deref()),
        "ip": r.text_of("ip"),
    })
}

fn parse_json_opt(raw: Option<&str>) -> serde_json::Value {
    match raw {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.to_string()))
        }
        _ => serde_json::Value::Null,
    }
}
