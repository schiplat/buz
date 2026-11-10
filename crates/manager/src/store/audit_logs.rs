//! Audit log search.

use sqlx::Row;

use shared::error::AppError;
use sqlx::SqlitePool;

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
    pool: &SqlitePool,
    filter: &AuditLogFilter,
) -> Result<(Vec<serde_json::Value>, i64), AppError> {
    let mut where_parts: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();

    if let Some(actor_id) = filter
        .restrict_actor_user_id
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        where_parts.push("actor_user_id = ?".to_string());
        binds.push(actor_id.to_string());
    }
    if let Some(a) = filter
        .action
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        where_parts.push("action = ?".to_string());
        binds.push(a.to_string());
    }
    if let Some(a) = filter
        .actor
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        where_parts.push("actor_username LIKE ?".to_string());
        binds.push(format!("%{a}%"));
    }
    if let Some(rt) = filter
        .resource_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        where_parts.push("resource_type = ?".to_string());
        binds.push(rt.to_string());
    }
    if let Some(from) = filter
        .from
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        where_parts.push("created_at >= ?".to_string());
        binds.push(from.to_string());
    }
    if let Some(to) = filter
        .to
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        where_parts.push("created_at <= ?".to_string());
        binds.push(to.to_string());
    }
    if let Some(search) = filter.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        where_parts.push(
            "(resource_name LIKE ? OR resource_id LIKE ? OR action LIKE ? OR actor_username LIKE ?)"
                .to_string(),
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
    let total = count_q.fetch_one(pool).await?;

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
    list_q = list_q.bind(filter.limit as i64).bind(filter.offset as i64);
    let rows = list_q.fetch_all(pool).await?;

    let items: Vec<serde_json::Value> = rows.into_iter().map(audit_row_json).collect();

    Ok((items, total))
}

fn audit_row_json(r: sqlx::sqlite::SqliteRow) -> serde_json::Value {
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
}

fn parse_json_opt(raw: Option<&str>) -> serde_json::Value {
    match raw {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.to_string()))
        }
        _ => serde_json::Value::Null,
    }
}
