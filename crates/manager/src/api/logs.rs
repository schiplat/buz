use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use futures_util::SinkExt;
use futures_util::StreamExt;
use serde::Deserialize;

use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::LogChunk;
use shared::rbac::Perm;
use tokio::sync::broadcast;

use crate::middleware::{AuthUser, RunnerClaims};
use crate::rbac::{self, pipeline_team_id};
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;
use crate::store::exec::DbPool;

async fn assert_job_team_view(
    state: &AppState,
    auth: &AuthUser,
    job_id: &str,
) -> Result<(), AppError> {
    let (pipeline_id, debug_unredact) =
        crate::store::logs::job_view_facts(&DbPool::from_sqlite(&state.pool), job_id)
            .await?
            .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    // Debug (no-redaction) jobs contain plaintext secrets in the logs; only
    // platform admins may read them, regardless of team membership.
    if debug_unredact != 0 && !auth.is_platform_admin {
        return Err(AppError::Forbidden(
            "this job was run in debug mode (unredacted logs) — only platform admins can view it"
                .into(),
        ));
    }

    let team_id = pipeline_team_id(&state.db(), &pipeline_id).await?;
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
    let owner = crate::store::logs::job_runner_id(&DbPool::from_sqlite(&state.pool), &job_id)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))?;
    // `owner` is `Option<String>`: a job that exists but was never dispatched
    // has a NULL runner_id, and no runner may write to it.
    if owner.as_deref() != Some(auth.runner_id.as_str()) {
        return Err(AppError::Forbidden(
            "job is not assigned to this runner".into(),
        ));
    }

    crate::store::logs::insert_chunk(
        &state.pool,
        &job_id,
        chunk.chunk_index,
        chunk.step_order,
        &chunk.content,
        chunk.level.as_ref().map(|l| l.as_str()),
    )
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
/// jobs page with `limit` (+ `offset`) from the front, or `tail` /
/// `before_step`+`before_chunk` from the end. Rows are append-only per
/// `(step_order, chunk_index)`, so both windows are stable.
#[derive(Debug, Deserialize, Default)]
pub struct LogsPageQuery {
    /// Max rows to return (1..=10_000).
    #[serde(default)]
    pub limit: Option<i64>,
    /// Rows to skip from the front (only meaningful with `limit`).
    #[serde(default)]
    pub offset: Option<i64>,
    /// Last N rows of the (cursor-bounded) set, returned in ascending order —
    /// the "show the last screen, load earlier on demand" read.
    #[serde(default)]
    pub tail: Option<i64>,
    /// End-exclusive cursor for `tail` / back-page reads: only rows strictly
    /// before `(before_step, before_chunk)` qualify. Both or neither.
    #[serde(default)]
    pub before_step: Option<i64>,
    #[serde(default)]
    pub before_chunk: Option<i64>,
}

pub async fn get_logs(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Query(page): Query<LogsPageQuery>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    assert_job_team_view(&state, &auth, &job_id).await?;

    let limit = page.limit.map(|l| l.clamp(1, 10_000));
    let offset = page.offset.unwrap_or(0).max(0);

    // End-of-log window: `tail` alone, or a back-page bounded by `limit`
    // when a `before` cursor is given. One query shape covers both.
    let has_cursor = page.before_step.is_some() || page.before_chunk.is_some();
    if has_cursor && (page.before_step.is_none() || page.before_chunk.is_none()) {
        return Err(AppError::ValidationFailed(
            "before_step and before_chunk must be given together".into(),
        ));
    }
    let window = match page.tail {
        Some(t) => Some(t.clamp(1, 10_000)),
        None if has_cursor => Some(limit.unwrap_or(10_000)),
        None => None,
    };

    let logs = if let Some(window) = window {
        crate::store::logs::read_windowed(
            &DbPool::from_sqlite(&state.pool),
            &job_id,
            page.before_step,
            page.before_chunk,
            window,
        )
        .await?
    } else if let Some(limit) = limit {
        crate::store::logs::read_page(&DbPool::from_sqlite(&state.pool), &job_id, limit, offset)
            .await?
    } else {
        crate::store::logs::read_all(&DbPool::from_sqlite(&state.pool), &job_id).await?
    };

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
    rbac::authenticate_user_token(&state.db(), &state.config.auth.jwt_secret, token_str).await
}

pub async fn stream_logs(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Query(query): Query<StreamQuery>,
) -> Result<impl IntoResponse, AppError> {
    let auth = auth_user_from_query_token(&state, query.token.as_deref()).await?;
    assert_job_team_view(&state, &auth, &job_id).await?;

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
    let team_id = pipeline_team_id(&state.db(), &pipeline_id).await?;
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
