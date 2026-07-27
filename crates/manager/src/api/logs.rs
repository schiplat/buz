use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use futures_util::SinkExt;
use futures_util::StreamExt;
use serde::Deserialize;
use shared::protocol::response::ApiResponse;
use shared::error::AppError;
use shared::protocol::LogChunk;
use shared::auth::TokenManager;
use sqlx::Row;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::api::AppState;
use crate::middleware::{AdminClaims, RunnerClaims};
use crate::services::log_broadcast::LogBroadcaster;

pub async fn upload(
    _auth: RunnerClaims,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(chunk): Json<LogChunk>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    sqlx::query(
        "INSERT OR REPLACE INTO job_logs (id, job_id, chunk_index, step_order, log_content, is_masked)
         VALUES (?, ?, ?, ?, ?, 1)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&job_id)
    .bind(chunk.chunk_index)
    .bind(chunk.step_order)
    .bind(&chunk.content)
    .execute(&state.pool)
    .await?;

    // Broadcast the log chunk to connected WebSocket clients
    state.log_broadcast.broadcast(&job_id, chunk.step_order, &chunk.content);

    Ok(Json(ApiResponse::success(())))
}

pub async fn get_logs(
    _auth: AdminClaims,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = sqlx::query(
        "SELECT chunk_index, step_order, log_content, created_at
         FROM job_logs WHERE job_id = ?
         ORDER BY step_order, chunk_index",
    )
    .bind(&job_id)
    .fetch_all(&state.pool)
    .await?;

    let logs: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "chunk_index": r.get::<i64, _>(0),
                "step_order": r.get::<i64, _>(1),
                "content": r.get::<String, _>(2),
                "created_at": r.get::<Option<String>, _>(3),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(logs)))
}

/// WebSocket endpoint that streams log chunks for a running job.
/// Auth: token passed as query parameter (`?token=...`).
#[derive(Deserialize)]
pub struct StreamQuery {
    token: Option<String>,
}

pub async fn stream_logs(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Query(query): Query<StreamQuery>,
) -> Result<impl IntoResponse, AppError> {
    // Validate JWT token
    let jwt_secret = &state.config.auth.jwt_secret;
    let token_str = query.token.as_deref().unwrap_or("");
    let token_mgr = TokenManager::new(jwt_secret);
    let claims = token_mgr
        .verify(token_str)
        .map_err(|_| AppError::AuthFailed("invalid or missing token".into()))?;

    if !matches!(claims.role, shared::auth::Role::Admin) {
        return Err(AppError::AuthFailed("admin privileges required".into()));
    }

    let broadcast = state.log_broadcast.clone();
    Ok(ws.on_upgrade(move |socket| handle_stream(socket, broadcast, job_id)))
}

async fn handle_stream(socket: WebSocket, broadcast: LogBroadcaster, job_id: String) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let mut rx = broadcast.subscribe(&job_id);

    let job_id_log = job_id.clone();

    // Replay any chunks buffered before this client connected
    for chunk in broadcast.replay_history(&job_id) {
        if ws_sender
            .send(Message::Text(chunk.into()))
            .await
            .is_err()
        {
            return;
        }
    }

    // Cancel on ping/pong/close
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    });

    // Forward broadcast chunks to WS
    let send_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(chunk) => {
                    if ws_sender
                        .send(Message::Text(chunk.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("WS client lagged by {n} chunks for job {job_id_log}");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Wait for either task to finish
    tokio::select! {
        _ = recv_task => {}
        _ = send_task => {}
    }

    broadcast.cleanup(&job_id);
}
