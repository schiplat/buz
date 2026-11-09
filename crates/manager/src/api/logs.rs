use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use futures_util::SinkExt;
use futures_util::StreamExt;
use serde::Deserialize;

use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{LogChunk, LogLevel};
use shared::rbac::Perm;
use sqlx::Row;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::middleware::{AuthUser, RunnerClaims};
use crate::rbac::{self, pipeline_team_id};
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;

async fn assert_job_team_view(
    pool: &sqlx::SqlitePool,
    auth: &AuthUser,
    job_id: &str,
) -> Result<(), AppError> {
    let row = sqlx::query("SELECT pipeline_id, debug_unredact FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))?;
    let pipeline_id: String = row.get(0);
    let debug_unredact: i64 = row.get(1);

    // Debug (no-redaction) jobs contain plaintext secrets in the logs; only
    // platform admins may read them, regardless of team membership.
    if debug_unredact != 0 && !auth.is_platform_admin {
        return Err(AppError::Forbidden(
            "this job was run in debug mode (unredacted logs) — only platform admins can view it"
                .into(),
        ));
    }

    let team_id = pipeline_team_id(pool, &pipeline_id).await?;
    auth.check_team(&team_id, Perm::TeamView)
}

pub async fn upload(
    auth: RunnerClaims,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(chunk): Json<LogChunk>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    // Runner↔job binding: log chunks are accepted only for jobs poll()
    // dispatched to this runner — otherwise any runner JWT could forge any
    // job's logs.
    let row = sqlx::query("SELECT runner_id FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))?;
    let owner: Option<String> = row.get(0);
    if owner.as_deref() != Some(auth.runner_id.as_str()) {
        return Err(AppError::Forbidden(
            "job is not assigned to this runner".into(),
        ));
    }

    sqlx::query(
        "INSERT OR REPLACE INTO job_logs (id, job_id, chunk_index, step_order, log_content, is_masked, level)
         VALUES (?, ?, ?, ?, ?, 1, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&job_id)
    .bind(chunk.chunk_index)
    .bind(chunk.step_order)
    .bind(&chunk.content)
    .bind(chunk.level.map(|l| l.as_str()))
    .execute(&state.pool)
    .await?;

    // Broadcast the log chunk to connected WebSocket clients
    state.log_broadcast.broadcast(
        &job_id,
        chunk.step_order,
        chunk.level.map(|l| l.as_str()),
        &chunk.content,
    );

    Ok(Json(ApiResponse::success(())))
}

/// Optional window over a job's log rows. Absent `limit` keeps the legacy
/// full read (the dashboard's job view relies on it); callers facing huge
/// jobs page with `limit` (+ `offset`). Rows are append-only, so
/// offset windows are stable.
#[derive(Debug, Deserialize, Default)]
pub struct LogsPageQuery {
    /// Max rows to return (1..=10_000).
    #[serde(default)]
    pub limit: Option<i64>,
    /// Rows to skip from the front (only meaningful with `limit`).
    #[serde(default)]
    pub offset: Option<i64>,
}

pub async fn get_logs(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Query(page): Query<LogsPageQuery>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    assert_job_team_view(&state.pool, &auth, &job_id).await?;

    let limit = page.limit.map(|l| l.clamp(1, 10_000));
    let offset = page.offset.unwrap_or(0).max(0);

    let rows = if let Some(limit) = limit {
        sqlx::query(
            "SELECT chunk_index, step_order, log_content, level, created_at
             FROM job_logs WHERE job_id = ?
             ORDER BY step_order, chunk_index
             LIMIT ? OFFSET ?",
        )
        .bind(&job_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query(
            "SELECT chunk_index, step_order, log_content, level, created_at
             FROM job_logs WHERE job_id = ?
             ORDER BY step_order, chunk_index",
        )
        .bind(&job_id)
        .fetch_all(&state.pool)
        .await?
    };

    let logs: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "chunk_index": r.get::<i64, _>(0),
                "step_order": r.get::<i64, _>(1),
                "content": r.get::<String, _>(2),
                "level": LogLevel::parse(r.get::<Option<String>, _>(3).as_deref()),
                "created_at": r.get::<Option<String>, _>(4),
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

/// Shared entry point for query-param auth: JWT **or** API token; both then
/// load a live user. Both current callers are GET endpoints on the
/// operations plane (job/pipeline log streams), so every token scope may use
/// them — call `AuthUser::assert_api_token_allowed` here if a future
/// query-token route leaves that envelope.
async fn auth_user_from_query_token(
    state: &AppState,
    token: Option<&str>,
) -> Result<AuthUser, AppError> {
    let token_str = token.unwrap_or("");
    if token_str.is_empty() {
        return Err(AppError::AuthFailed("invalid or missing token".into()));
    }
    rbac::authenticate_user_token(&state.pool, &state.config.auth.jwt_secret, token_str).await
}

pub async fn stream_logs(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Query(query): Query<StreamQuery>,
) -> Result<impl IntoResponse, AppError> {
    let auth = auth_user_from_query_token(&state, query.token.as_deref()).await?;
    assert_job_team_view(&state.pool, &auth, &job_id).await?;

    let broadcast = state.log_broadcast.clone();
    Ok(ws.on_upgrade(move |socket| handle_stream(socket, broadcast, job_id)))
}

async fn handle_stream(socket: WebSocket, broadcast: LogBroadcaster, job_id: String) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let mut rx = broadcast.subscribe(&job_id);

    let job_id_log = job_id.clone();

    // Replay any chunks buffered before this client connected
    for chunk in broadcast.replay_history(&job_id) {
        if ws_sender.send(Message::Text(chunk.into())).await.is_err() {
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
                    if ws_sender.send(Message::Text(chunk.into())).await.is_err() {
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

/// True when a pipeline-stream status message describes a debug (unredacted)
/// job. Non-admins must not even learn such a job exists — the pipeline
/// WebSocket drops these messages on the subscriber side, matching the
/// REST-level debug filtering on `list` / `get_one`.
pub fn is_debug_status_message(msg: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(msg)
        .map(|v| {
            v.get("data")
                .and_then(|d| d.get("debug_unredact"))
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// WebSocket endpoint that streams job status updates for all jobs
/// within a pipeline (PipelineDetail build-history live refresh).
pub async fn pipeline_stream(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(pipeline_id): Path<String>,
    Query(query): Query<StreamQuery>,
) -> Result<impl IntoResponse, AppError> {
    let auth = auth_user_from_query_token(&state, query.token.as_deref()).await?;
    let team_id = pipeline_team_id(&state.pool, &pipeline_id).await?;
    auth.check_team(&team_id, Perm::TeamView)?;

    let broadcast = state.log_broadcast.clone();
    let is_platform_admin = auth.is_platform_admin;
    Ok(ws.on_upgrade(move |socket| {
        handle_pipeline_stream(socket, broadcast, pipeline_id, is_platform_admin)
    }))
}

async fn handle_pipeline_stream(
    socket: WebSocket,
    broadcast: LogBroadcaster,
    pipeline_id: String,
    is_platform_admin: bool,
) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let mut rx = broadcast.pipeline_subscribe(&pipeline_id);
    let pid_log = pipeline_id.clone();

    // Replay recent status history
    for chunk in broadcast.pipeline_replay(&pipeline_id) {
        if !is_platform_admin && is_debug_status_message(&chunk) {
            continue;
        }
        if ws_sender.send(Message::Text(chunk.into())).await.is_err() {
            return;
        }
    }

    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    });

    let send_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(chunk) => {
                    if !is_platform_admin && is_debug_status_message(&chunk) {
                        continue;
                    }
                    if ws_sender.send(Message::Text(chunk.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Pipeline WS lagged by {n} msgs for {pid_log}");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    tokio::select! {
        _ = recv_task => {}
        _ = send_task => {}
    }

    broadcast.pipeline_cleanup(&pipeline_id);
}
