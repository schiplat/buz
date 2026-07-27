use axum::{Json, extract::{State, Path}};
use shared::protocol::{CreateHostRequest, UpdateHostRequest};
use shared::protocol::response::ApiResponse;
use shared::error::AppError;
use sqlx::Row;
use uuid::Uuid;

use crate::api::AppState;
use crate::middleware::AdminClaims;

pub async fn list(
    _auth: AdminClaims,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = sqlx::query(
        "SELECT id, name, host, port, username, credential_type, credential_ref, labels, created_at, updated_at
         FROM hosts ORDER BY created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;

    let hosts: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let labels_raw: String = r.get(7);
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "name": r.get::<String, _>(1),
                "host": r.get::<String, _>(2),
                "port": r.get::<i64, _>(3) as u16,
                "username": r.get::<String, _>(4),
                "credential_type": r.get::<String, _>(5),
                "credential_ref": r.get::<String, _>(6),
                "labels": serde_json::from_str::<Vec<String>>(&labels_raw).unwrap_or_default(),
                "created_at": r.get::<Option<String>, _>(8),
                "updated_at": r.get::<Option<String>, _>(9),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(hosts)))
}

pub async fn create(
    _auth: AdminClaims,
    State(state): State<AppState>,
    Json(req): Json<CreateHostRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let id = Uuid::new_v4().to_string();
    let cred_type = req.credential_type.unwrap_or_else(|| "key_path".into());
    let labels_json = serde_json::to_string(
        &req.labels.unwrap_or_default(),
    )
    .unwrap_or_else(|_| "[]".into());

    sqlx::query(
        "INSERT INTO hosts (id, name, host, port, username, credential_type, credential_ref, labels)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&req.name)
    .bind(&req.host)
    .bind(req.port as i64)
    .bind(&req.username)
    .bind(&cred_type)
    .bind(&req.credential_ref)
    .bind(&labels_json)
    .execute(&state.pool)
    .await?;

    tracing::info!("Host created: {} ({id})", req.name);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": req.name,
    }))))
}

pub async fn get_one(
    _auth: AdminClaims,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let r = sqlx::query(
        "SELECT id, name, host, port, username, credential_type, credential_ref, labels, created_at, updated_at
         FROM hosts WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("host not found".into()))?;

    let labels_raw: String = r.get(7);
    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.get::<String, _>(0),
        "name": r.get::<String, _>(1),
        "host": r.get::<String, _>(2),
        "port": r.get::<i64, _>(3) as u16,
        "username": r.get::<String, _>(4),
        "credential_type": r.get::<String, _>(5),
        "credential_ref": r.get::<String, _>(6),
        "labels": serde_json::from_str::<Vec<String>>(&labels_raw).unwrap_or_default(),
        "created_at": r.get::<Option<String>, _>(8),
        "updated_at": r.get::<Option<String>, _>(9),
    }))))
}

pub async fn update(
    _auth: AdminClaims,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateHostRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    // Verify the host exists
    let _existing = sqlx::query("SELECT id FROM hosts WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("host not found".into()))?;

    if let Some(labels) = &req.labels {
        let labels_json = serde_json::to_string(labels).unwrap_or_else(|_| "[]".into());
        sqlx::query("UPDATE hosts SET labels = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(&labels_json)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    if let Some(ref host) = req.host {
        sqlx::query("UPDATE hosts SET host = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(host)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    if let Some(port) = req.port {
        sqlx::query("UPDATE hosts SET port = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(port as i64)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    if let Some(ref username) = req.username {
        sqlx::query("UPDATE hosts SET username = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(username)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    if let Some(ref ct) = req.credential_type {
        sqlx::query("UPDATE hosts SET credential_type = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(ct)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    if let Some(ref cr) = req.credential_ref {
        sqlx::query("UPDATE hosts SET credential_ref = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(cr)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    if let Some(ref n) = req.name {
        sqlx::query("UPDATE hosts SET name = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(n)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    tracing::info!("Host updated: {id}");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "updated": true,
    }))))
}

pub async fn delete(
    _auth: AdminClaims,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let result = sqlx::query("DELETE FROM hosts WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("host not found".into()));
    }

    tracing::info!("Host deleted: {id}");
    Ok(Json(ApiResponse::success(())))
}
