use axum::extract::ConnectInfo;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::{extract::FromRequestParts, extract::State, response::IntoResponse, Json};
use shared::auth::TokenManager;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{
    CreateRunnerRequest, ExecutionBackendStatus, HeartbeatRequest, HeartbeatResponse,
    RegisterRequest, RegisterResponse, RunnerExecutionMode, RunnerPolicyConfig, SystemInfo,
    UpdateRunnerRequest,
};
use std::convert::Infallible;
use std::net::SocketAddr;

use crate::audit::{self, Action, AuditEvent, Resource};
use crate::config::Config;
use crate::crypto;
use crate::middleware::{PlatformAdmin, RunnerClaims};
use crate::state::AppState;
use sqlx::Row;
use uuid::Uuid;

/// Peer address when the server is served with `into_make_service_with_connect_info`.
/// Missing ConnectInfo → `None` (treated as non-loopback for standalone checks).
pub struct ClientAddr(pub Option<SocketAddr>);

impl<S> FromRequestParts<S> for ClientAddr
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(ClientAddr(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(addr)| *addr),
        ))
    }
}

/// True when the runner has **every** label required by the pipeline/job
/// (intersection / "must match ALL"). Empty `required` matches any runner.
pub fn runner_satisfies_labels(runner_labels: &[String], required: &[String]) -> bool {
    required
        .iter()
        .all(|need| runner_labels.iter().any(|have| have == need))
}

/// Best-effort note of why a poller skipped a pending job (slot wait,
/// profile not offered, host mode). Display-only; cleared on claim. Logged
/// only when the reason changes, so a job parked on one reason does not
/// repeat itself on every poll round.
async fn record_dispatch_reason(pool: &sqlx::SqlitePool, job_id: &str, reason: &str) {
    match sqlx::query(
        "UPDATE jobs SET dispatch_reason = ? WHERE id = ? AND status = 'pending'
         AND COALESCE(dispatch_reason, '') != ?",
    )
    .bind(reason)
    .bind(job_id)
    .bind(reason)
    .execute(pool)
    .await
    {
        Ok(done) if done.rows_affected() > 0 => {
            tracing::info!(job = %job_id, reason, "job not dispatched");
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(job = %job_id, error = %e, "failed to record dispatch reason");
        }
    }
}

/// `(resource_profile, team_id)` frozen into the job snapshot at trigger.
/// An empty profile means the implicit `default`.
fn job_resource_request(snapshot_raw: &str) -> (String, Option<String>) {
    let snapshot: serde_json::Value = serde_json::from_str(snapshot_raw).unwrap_or_default();
    let profile = snapshot
        .get("resource_profile")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let team = snapshot
        .get("team_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    (profile, team)
}

/// Prefer frozen job snapshot `runner_labels`; fall back to live pipeline labels JSON.
pub fn required_labels_for_job(snapshot_json: &str, pipeline_labels_json: &str) -> Vec<String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(snapshot_json) {
        if let Some(arr) = v.get("runner_labels").and_then(|x| x.as_array()) {
            return arr
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect();
        }
    }
    serde_json::from_str(pipeline_labels_json).unwrap_or_default()
}

/// Insert a runner event (fire-and-forget; failures are silently logged).
async fn log_runner_event(
    pool: &sqlx::SqlitePool,
    runner_id: &str,
    event_type: &str,
    message: &str,
) {
    let id = Uuid::new_v4().to_string();
    if let Err(e) = sqlx::query(
        "INSERT INTO runner_events (id, runner_id, event_type, message) VALUES (?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(runner_id)
    .bind(event_type)
    .bind(message)
    .execute(pool)
    .await
    {
        tracing::warn!("Failed to log runner event for {runner_id}: {e}");
    }
}

/// Validate and clamp max_concurrent_jobs to 1..=32.
pub fn normalize_max_concurrent(v: Option<u32>) -> Result<u32, AppError> {
    let n = v.unwrap_or(1);
    if !(1..=32).contains(&n) {
        return Err(AppError::ValidationFailed(
            "max_concurrent_jobs must be between 1 and 32".into(),
        ));
    }
    Ok(n)
}

/// Register-token check, constant-time. The DB stores SHA-256(token) for rows
/// created after the hash-only change; legacy rows may still carry the
/// plaintext UUID, so both forms are accepted (rows converge on re-issue).
/// Timing on the comparison must not leak how much of the token matched.
fn register_token_matches(stored: &str, provided: &str) -> bool {
    if provided.is_empty() || stored.is_empty() {
        return false;
    }
    shared::api_token::ct_eq(stored, &shared::api_token::hash_token(provided))
        || shared::api_token::ct_eq(stored, provided)
}

/// Re-registration proof: a Bearer token that is a signature-valid runner JWT
/// (expiry ignored — refresh semantics) whose `sub` matches the runner row.
/// Knowing the runner name alone is never enough to obtain a fresh JWT.
fn bearer_proves_runner(
    headers: &axum::http::HeaderMap,
    jwt_secret: &str,
    runner_id: &str,
) -> bool {
    let Some(bearer) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
    else {
        return false;
    };
    match TokenManager::new(jwt_secret).verify_ignoring_exp(bearer) {
        Ok(claims) => claims.role == shared::auth::Role::Runner && claims.sub == runner_id,
        Err(_) => false,
    }
}

/// Validate and default the claim strategy (`backfill` | `fifo`).
pub fn normalize_claim_strategy(v: Option<String>) -> Result<String, AppError> {
    let raw = v.unwrap_or_default();
    crate::services::slot_scheduler::ClaimStrategy::parse(&raw)
        .map(|s| s.as_str().to_string())
        .ok_or_else(|| {
            AppError::ValidationFailed("claim_strategy must be 'backfill' or 'fifo'".into())
        })
}

pub fn normalize_ssh_host_key_policy(v: Option<String>) -> Result<String, AppError> {
    let p = v.unwrap_or_else(|| "known_hosts".into());
    match p.as_str() {
        "insecure" | "known_hosts" => Ok(p),
        _ => Err(AppError::ValidationFailed(
            "ssh_host_key_policy must be 'insecure' or 'known_hosts'".into(),
        )),
    }
}

fn bool_i64(v: Option<bool>, default: bool) -> i64 {
    if v.unwrap_or(default) {
        1
    } else {
        0
    }
}

fn i64_bool(v: i64) -> bool {
    v != 0
}

pub fn parse_execution_mode(
    value: String,
) -> Result<shared::protocol::RunnerExecutionMode, AppError> {
    shared::protocol::RunnerExecutionMode::parse(&value).ok_or_else(|| {
        AppError::ValidationFailed(format!(
            "invalid runner execution_mode '{value}' (expected host | container | sandbox | vm)"
        ))
    })
}

async fn validate_runner_team(
    pool: &sqlx::SqlitePool,
    team_id: Option<&str>,
) -> Result<Option<String>, AppError> {
    let Some(team_id) = team_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM teams WHERE id = ?)")
        .bind(team_id)
        .fetch_one(pool)
        .await?;
    if !exists {
        return Err(AppError::ValidationFailed(format!(
            "team '{team_id}' does not exist"
        )));
    }
    Ok(Some(team_id.to_string()))
}

/// Accept a runner-reported deployment identity only when it validates;
/// otherwise return empty so the stored value is kept (COALESCE pattern) and
/// the report is logged. A malformed id must not fail a register/heartbeat.
fn sanitized_instance_id(raw: &str, what: &str, runner: &str) -> String {
    match shared::instance::validate(raw) {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!(%error, what, runner, "runner reported an invalid instance id; ignoring");
            String::new()
        }
    }
}

/// Load UI policy columns for a runner.
async fn load_runner_policy(
    pool: &sqlx::SqlitePool,
    runner_id: &str,
) -> Result<RunnerPolicyConfig, AppError> {
    let row = sqlx::query(
        "SELECT max_concurrent_jobs, clean_workspace_after_job,
                shell_enabled, ssh_enabled, docker_enabled, wasm_enabled, ssh_host_key_policy,
                execution_mode, claim_strategy, netns_isolation
         FROM runners WHERE id = ?",
    )
    .bind(runner_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("runner not found".into()))?;

    Ok(RunnerPolicyConfig {
        max_concurrent_jobs: row.get::<i64, _>(0).max(1) as u32,
        clean_workspace_after_job: i64_bool(row.get(1)),
        shell_enabled: i64_bool(row.get(2)),
        ssh_enabled: i64_bool(row.get(3)),
        docker_enabled: i64_bool(row.get(4)),
        wasm_enabled: i64_bool(row.get(5)),
        ssh_host_key_policy: {
            let p: String = row.get(6);
            if p == "insecure" {
                p
            } else {
                "known_hosts".into()
            }
        },
        execution_mode: parse_execution_mode(row.get::<String, _>(7))?,
        claim_strategy: normalize_claim_strategy(Some(row.get::<String, _>(8)))
            .unwrap_or_else(|_| "backfill".into()),
        netns_isolation: i64_bool(row.get(9)),
    })
}

fn policy_json(p: &RunnerPolicyConfig) -> serde_json::Value {
    serde_json::json!({
        "max_concurrent_jobs": p.max_concurrent_jobs,
        "clean_workspace_after_job": p.clean_workspace_after_job,
        "execution_mode": p.execution_mode.as_str(),
        "shell_enabled": p.shell_enabled,
        "ssh_enabled": p.ssh_enabled,
        "docker_enabled": p.docker_enabled,
        "wasm_enabled": p.wasm_enabled,
        "ssh_host_key_policy": p.ssh_host_key_policy,
        "claim_strategy": p.claim_strategy,
        "netns_isolation": p.netns_isolation,
    })
}

pub async fn create_runner(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<CreateRunnerRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let created = create_runner_record(&state.pool, &req).await?;
    let team_id: Option<String> = sqlx::query_scalar("SELECT team_id FROM runners WHERE id = ?")
        .bind(&created.id)
        .fetch_one(&state.pool)
        .await?;

    let policy = load_runner_policy(&state.pool, &created.id).await?;
    // The register token is deliberately absent from the snapshot.
    audit::record(
        &state.pool,
        AuditEvent::new(Action::RunnerCreate, &auth)
            .on(Resource::runner(&created.id, &created.name))
            .team(team_id.clone())
            .after(Some(serde_json::json!({
                "name": created.name,
                "team_id": team_id,
                "labels": req.labels,
                "max_concurrent_jobs": created.max_concurrent_jobs,
                "execution_mode": policy.execution_mode.as_str(),
            }))),
    )
    .await;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": created.id,
        "name": created.name,
        "team_id": team_id,
        "register_token": created.register_token,
        "max_concurrent_jobs": created.max_concurrent_jobs,
        "policy": policy_json(&policy),
    }))))
}

/// Core runner creation used by both the HTTP handler and the
/// `gen-runner-token` CLI subcommand. Persists the runner and returns its
/// one-shot register token.
pub async fn create_runner_record(
    pool: &sqlx::SqlitePool,
    req: &CreateRunnerRequest,
) -> Result<CreatedRunner, AppError> {
    let id = Uuid::new_v4().to_string();
    let register_token = Uuid::new_v4().to_string();
    let labels_json = serde_json::to_string(req.labels.as_deref().unwrap_or_default())
        .unwrap_or_else(|_| "[]".into());
    let max_concurrent = normalize_max_concurrent(req.max_concurrent_jobs)?;
    let ssh_policy = normalize_ssh_host_key_policy(req.ssh_host_key_policy.clone())?;
    let claim_strategy = normalize_claim_strategy(req.claim_strategy.clone())?;
    let clean = bool_i64(req.clean_workspace_after_job, false);
    let shell = bool_i64(req.shell_enabled, true);
    let ssh = bool_i64(req.ssh_enabled, true);
    let docker = bool_i64(req.docker_enabled, false);
    let wasm = bool_i64(req.wasm_enabled, false);
    let team_id = validate_runner_team(pool, req.team_id.as_deref()).await?;
    let execution_mode = req.execution_mode.unwrap_or_default();
    if execution_mode != RunnerExecutionMode::Host {
        return Err(AppError::Conflict(
            "new runners start in Host mode; register and report sandbox capabilities before enabling Container or VM".into(),
        ));
    }
    let execution_mode = execution_mode.as_str();

    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, version, team_id, execution_mode,
         max_concurrent_jobs, clean_workspace_after_job,
         shell_enabled, ssh_enabled, docker_enabled, wasm_enabled, ssh_host_key_policy, claim_strategy)
         VALUES (?, ?, ?, ?, 'offline', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&req.name)
    // Hash-only at rest: the plaintext is returned exactly once in the API
    // response and never stored (same discipline as buz_/buzsd_ tokens).
    .bind(shared::api_token::hash_token(&register_token))
    .bind(&labels_json)
    .bind(&req.version)
    .bind(&team_id)
    .bind(execution_mode)
    .bind(max_concurrent as i64)
    .bind(clean)
    .bind(shell)
    .bind(ssh)
    .bind(docker)
    .bind(wasm)
    .bind(&ssh_policy)
    .bind(&claim_strategy)
    .execute(pool)
    .await?;

    tracing::info!(
        name = %req.name,
        id = %id,
        max_concurrent = max_concurrent,
        "runner created (register_token returned once in API response, not logged)"
    );

    log_runner_event(
        pool,
        &id,
        "created",
        &format!("Runner created, awaiting registration (max_concurrent={max_concurrent})"),
    )
    .await;

    Ok(CreatedRunner {
        id,
        name: req.name.clone(),
        register_token,
        max_concurrent_jobs: max_concurrent,
    })
}

/// Result of `create_runner_record` — identifier, name, and one-shot token.
pub struct CreatedRunner {
    pub id: String,
    pub name: String,
    pub register_token: String,
    pub max_concurrent_jobs: u32,
}

/// Ensure `standalone.auto_approve` runner rows exist (Manager-side authorization).
/// No-op when standalone mode is disabled.
pub async fn ensure_standalone_auto_approve(
    pool: &sqlx::SqlitePool,
    config: &Config,
) -> anyhow::Result<()> {
    if !config.standalone.enabled {
        return Ok(());
    }

    tracing::warn!(
        auto_approve = ?config.standalone.auto_approve,
        "STANDALONE DEV MODE enabled — rejecting non-loopback runner registration; \
         tokenless register allowed only from loopback for authorized runner names"
    );

    for name in &config.standalone.auto_approve {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(1) FROM runners WHERE name = ?")
            .bind(name)
            .fetch_one(pool)
            .await?;
        if exists > 0 {
            tracing::info!(runner = %name, "standalone auto_approve: runner already exists");
            continue;
        }
        let req = CreateRunnerRequest {
            name: name.to_string(),
            labels: Some(vec!["standalone".into()]),
            team_id: None,
            execution_mode: None,
            version: Some(shared::version::VERSION.to_string()),
            max_concurrent_jobs: None,
            clean_workspace_after_job: None,
            shell_enabled: None,
            ssh_enabled: None,
            docker_enabled: None,
            wasm_enabled: None,
            ssh_host_key_policy: None,
            claim_strategy: None,
        };
        match create_runner_record(pool, &req).await {
            Ok(created) => {
                tracing::info!(
                    runner = %created.name,
                    id = %created.id,
                    "standalone auto_approve: created authorized runner"
                );
            }
            Err(e) if e.to_string().contains("UNIQUE constraint failed") => {
                tracing::info!(runner = %name, "standalone auto_approve: raced create, already exists");
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "standalone auto_approve failed for '{name}': {e}"
                ));
            }
        }
    }
    Ok(())
}

fn peer_is_loopback(peer: Option<SocketAddr>) -> bool {
    peer.map(|addr| addr.ip().is_loopback()).unwrap_or(false)
}

pub async fn register(
    ClientAddr(peer): ClientAddr,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<ApiResponse<RegisterResponse>>, AppError> {
    let name = req.name.trim();
    let register_token = req.register_token.trim();
    let standalone = state.config.standalone.enabled;
    let loopback = peer_is_loopback(peer);

    // Standalone: never accept runners from outside the host network namespace.
    if standalone && !loopback {
        tracing::warn!(
            runner = %name,
            ?peer,
            "standalone mode rejected non-loopback runner registration"
        );
        return Err(AppError::Forbidden(
            "standalone mode rejects non-loopback runner registration".into(),
        ));
    }

    // Look up the runner by name first.
    let existing = sqlx::query("SELECT id, register_token, token FROM runners WHERE name = ?")
        .bind(name)
        .fetch_optional(&state.pool)
        .await?;

    let runner_id: String;

    if let Some(ref row) = existing {
        let existing_token: Option<String> = row.get(2);
        let row_id: String = row.get(0);

        if existing_token.as_ref().is_none_or(|t| t.is_empty()) {
            // First-time registration — must match the one-shot register_token.
            let stored_rt: String = row.get(1);
            if !register_token_matches(&stored_rt, register_token) {
                // Standalone + loopback + authorized name (row exists): allow
                // tokenless / mismatched token. Production path unchanged.
                if standalone && loopback {
                    tracing::info!(
                        runner = %name,
                        "standalone loopback register without token (authorized)"
                    );
                } else {
                    tracing::warn!(
                        runner = %name,
                        labels = ?req.labels,
                        version = ?req.version,
                        "Runner registration failed: invalid register token"
                    );
                    return Err(AppError::AuthFailed(
                        "invalid register token or runner name".into(),
                    ));
                }
            }
        } else {
            // Re-registration (runner already holds a JWT). Identity must be
            // proven — knowing the runner NAME alone must not be enough:
            //   (a) standalone + loopback trust boundary, or
            //   (b) Bearer = the runner's previous JWT (signature valid,
            //       expiry ignored, role Runner, sub matches this runner), or
            //   (c) an admin-issued register_token (stored value must match —
            //       recovery path after the cached JWT is lost or the
            //       jwt_secret was rotated).
            let authorized = if standalone && loopback {
                true
            } else {
                bearer_proves_runner(&headers, &state.config.auth.jwt_secret, &row_id) || {
                    let stored_rt: String = row.get(1);
                    register_token_matches(&stored_rt, register_token)
                }
            };
            if !authorized {
                tracing::warn!(
                    runner = %name,
                    ?peer,
                    "Runner re-registration rejected: no valid bearer JWT or register token"
                );
                return Err(AppError::AuthFailed(
                    "invalid register token or runner name".into(),
                ));
            }
        }

        runner_id = row_id;
    } else {
        if standalone && loopback {
            tracing::warn!(
                runner = %name,
                "standalone register rejected: runner not authorized (create in UI or auto_approve)"
            );
            return Err(AppError::Forbidden(
                "standalone: runner not authorized — create it in Manager UI (New Runner) \
                 or add the name to standalone.auto_approve"
                    .into(),
            ));
        }
        tracing::warn!(
            runner = %name,
            labels = ?req.labels,
            version = ?req.version,
            "Runner registration failed: unknown runner name (create the runner in UI first)"
        );
        return Err(AppError::AuthFailed(
            "invalid register token or runner name".into(),
        ));
    }

    // BUZ_RUNNER_ID pinning: a deployment can pre-declare which runner row it
    // intends to register as. A mismatch is config drift — reject rather than
    // binding to the wrong row. The pin is correlation data, not an auth
    // input: authorization still flows through name + register token / bearer
    // JWT exactly as before.
    let pinned_id = req.runner_id.trim();
    if !pinned_id.is_empty() && pinned_id != runner_id {
        tracing::warn!(
            runner = %name,
            pinned = %pinned_id,
            actual = %runner_id,
            ?peer,
            "Runner registration rejected: BUZ_RUNNER_ID does not match this runner"
        );
        return Err(AppError::Conflict(format!(
            "BUZ_RUNNER_ID '{pinned_id}' does not match this runner ('{runner_id}')"
        )));
    }

    let token_mgr = TokenManager::new(&state.config.auth.jwt_secret);
    let runner_jwt = token_mgr
        .create_token(
            &runner_id,
            shared::auth::Role::Runner,
            state.config.auth.runner_register_token_ttl_hours,
        )
        .map_err(|e| AppError::Internal(format!("token generation failed: {e}")))?;

    // Labels are UI-managed — do not overwrite from the agent register payload.
    let version = req.version.as_deref().filter(|v| !v.is_empty());
    // Deployment identities are stored when reported (older Runners omit them);
    // malformed values are dropped with a warning rather than rejecting the
    // registration — identity is correlation data, not an auth input.
    let host_id = sanitized_instance_id(&req.host_id, "host_id", name);
    let runner_instance_id =
        sanitized_instance_id(&req.runner_instance_id, "runner_instance_id", name);

    // Hash-only at rest: the runner JWT is never stored in plaintext — only
    // its SHA-256, kept for the "already holds a token" state. The JWT itself
    // is stateless (signature-verified per request) and returned to the agent.
    sqlx::query(
        "UPDATE runners SET token = ?, register_token = '', status = 'online', \
         version = COALESCE(?, version), \
         host_id = COALESCE(NULLIF(?, ''), host_id), \
         runner_instance_id = COALESCE(NULLIF(?, ''), runner_instance_id), \
         last_seen = datetime('now'), updated_at = datetime('now'), \
         restart_requested = 0 WHERE id = ?",
    )
    .bind(shared::api_token::hash_token(&runner_jwt))
    .bind(version)
    .bind(&host_id)
    .bind(&runner_instance_id)
    .bind(&runner_id)
    .execute(&state.pool)
    .await?;

    tracing::info!(runner = %name, host_id = %host_id, instance = %runner_instance_id, "Runner registered: {} ({runner_id})", name);

    let policy = load_runner_policy(&state.pool, &runner_id).await?;
    let labels_db: String = sqlx::query_scalar("SELECT labels FROM runners WHERE id = ?")
        .bind(&runner_id)
        .fetch_one(&state.pool)
        .await
        .unwrap_or_else(|_| "[]".into());

    log_runner_event(
        &state.pool,
        &runner_id,
        "registered",
        &format!(
            "Runner registered (version: {}, labels: {})",
            req.version.unwrap_or_default(),
            labels_db
        ),
    )
    .await;

    Ok(Json(ApiResponse::success(RegisterResponse {
        runner_id,
        token: runner_jwt,
        policy,
    })))
}

pub async fn heartbeat(
    auth: RunnerClaims,
    State(state): State<AppState>,
    Json(req): Json<HeartbeatRequest>,
) -> Result<impl IntoResponse, AppError> {
    let system_info_json = req
        .system_info
        .as_ref()
        .and_then(|si| serde_json::to_string(si).ok())
        .unwrap_or_else(|| "{}".into());
    let version = req
        .system_info
        .as_ref()
        .map(|si| si.runner_version.as_str())
        .filter(|v| !v.is_empty());

    // Keep the last reported deployment identities; empty/malformed values
    // leave the stored ones untouched.
    let host_id = sanitized_instance_id(&req.host_id, "host_id", &auth.runner_id);
    let runner_instance_id = sanitized_instance_id(
        &req.runner_instance_id,
        "runner_instance_id",
        &auth.runner_id,
    );
    // The sandboxd instance this Runner is attached to, derived from the
    // heartbeat's sandbox block so the claim path can group slot usage by host
    // without parsing the system_info blob.
    let sandbox_instance_id = req
        .system_info
        .as_ref()
        .and_then(|si| si.sandboxd.as_ref())
        .and_then(|b| b.get("instance_id"))
        .and_then(serde_json::Value::as_str)
        .map(|raw| sanitized_instance_id(raw, "sandbox_instance_id", &auth.runner_id))
        .unwrap_or_default();
    sqlx::query(
        "UPDATE runners SET status = ?, system_info = ?, version = COALESCE(?, version), \
         host_id = COALESCE(NULLIF(?, ''), host_id), \
         runner_instance_id = COALESCE(NULLIF(?, ''), runner_instance_id), \
         sandbox_instance_id = COALESCE(NULLIF(?, ''), sandbox_instance_id), \
         last_seen = datetime('now'), updated_at = datetime('now') WHERE id = ?",
    )
    .bind(&req.status)
    .bind(&system_info_json)
    .bind(version)
    .bind(&host_id)
    .bind(&runner_instance_id)
    .bind(&sandbox_instance_id)
    .bind(&auth.runner_id)
    .execute(&state.pool)
    .await?;

    let policy = load_runner_policy(&state.pool, &auth.runner_id).await?;

    // Serve a pending one-shot restart command, then clear it (serve-and-consume).
    let restart_requested: bool =
        sqlx::query_scalar("SELECT restart_requested FROM runners WHERE id = ?")
            .bind(&auth.runner_id)
            .fetch_one(&state.pool)
            .await
            .map(|v: i64| v != 0)
            .unwrap_or(false);

    if restart_requested {
        tracing::info!(runner = %auth.runner_id, "Serving restart command via heartbeat");
        sqlx::query("UPDATE runners SET restart_requested = 0 WHERE id = ?")
            .bind(&auth.runner_id)
            .execute(&state.pool)
            .await?;
        log_runner_event(
            &state.pool,
            &auth.runner_id,
            "restart_served",
            "Restart command delivered to runner",
        )
        .await;
    }

    // Serve a pending one-shot capability re-probe command, then clear it
    // (serve-and-consume). Like restart, the Runner sees it on its next
    // heartbeat (≤30s) and probes immediately instead of idling.
    let reprobe_requested: bool =
        sqlx::query_scalar("SELECT capability_reprobe_requested FROM runners WHERE id = ?")
            .bind(&auth.runner_id)
            .fetch_one(&state.pool)
            .await
            .map(|v: i64| v != 0)
            .unwrap_or(false);

    if reprobe_requested {
        tracing::info!(runner = %auth.runner_id, "Serving capability re-probe command via heartbeat");
        sqlx::query(
            "UPDATE runners SET capability_reprobe_requested = 0,
                capability_reprobe_served_at = datetime('now') WHERE id = ?",
        )
        .bind(&auth.runner_id)
        .execute(&state.pool)
        .await?;
        log_runner_event(
            &state.pool,
            &auth.runner_id,
            "capability_reprobe_served",
            "Capability re-probe command delivered to runner",
        )
        .await;
    }

    Ok((
        StatusCode::OK,
        Json(ApiResponse::success(HeartbeatResponse {
            policy,
            restart: restart_requested,
            reprobe: reprobe_requested,
        })),
    ))
}

/// Long-poll for a task or cancellation. Subscribe before checking the DB so
/// a commit racing the check is buffered by broadcast and cannot be missed.
pub async fn poll(
    auth: RunnerClaims,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<ApiResponse<Option<serde_json::Value>>>, AppError> {
    let long_poll = headers
        .get("prefer")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(',')
                .map(str::trim)
                .find_map(|item| item.strip_prefix("wait="))
        })
        .and_then(|v| v.parse::<u64>().ok())
        .map(|seconds| std::time::Duration::from_secs(seconds.min(45)))
        .unwrap_or_default();
    let active_jobs = headers
        .get("x-buz-active-jobs")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    let acks: Vec<String> = headers
        .get("x-buz-cancel-ack")
        .and_then(|v| v.to_str().ok())
        .map(|raw| {
            raw.split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    poll_with_wait(auth, state, active_jobs, acks, long_poll).await
}

pub async fn poll_with_wait(
    auth: RunnerClaims,
    state: AppState,
    active_jobs: i64,
    mut pending_acks: Vec<String>,
    wait: std::time::Duration,
) -> Result<Json<ApiResponse<Option<serde_json::Value>>>, AppError> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let mut events = crate::services::poll_events::subscribe();
        let response = poll_once(auth.clone(), state.clone(), &pending_acks, active_jobs).await?;
        pending_acks.clear();

        let payload = response.0.data.as_ref().and_then(|value| value.as_ref());
        let has_job = payload.and_then(|v| v.get("job_id")).is_some();
        let has_cancels = payload
            .and_then(|v| v.get("cancelled_jobs"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|jobs| !jobs.is_empty());
        if has_job || has_cancels {
            return Ok(response);
        }

        // Frozen/restarting runners must stop dispatch immediately; restart is
        // also signalled through the restart API, but don't hold up its response.
        let (frozen, restarting): (i64, i64) =
            sqlx::query_as("SELECT is_frozen, restart_requested FROM runners WHERE id = ?")
                .bind(&auth.runner_id)
                .fetch_optional(&state.pool)
                .await?
                .unwrap_or((0, 0));
        if frozen != 0 || restarting != 0 {
            return Ok(response);
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(response);
        }
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Ok(())) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) | Err(_) => {
                return Ok(response)
            }
        }
    }
}

async fn poll_once(
    auth: RunnerClaims,
    state: AppState,
    cancel_acks: &[String],
    active_jobs: i64,
) -> Result<Json<ApiResponse<Option<serde_json::Value>>>, AppError> {
    acknowledge_cancelled(&state.pool, &auth.runner_id, cancel_acks).await?;
    let cancelled_jobs = pending_cancelled_for_delivery(&state.pool, &auth.runner_id).await?;

    // Reject frozen runners — they should not be dispatched work.
    // Also skip dispatch while a restart is pending: otherwise the window between
    // the restart request and the next heartbeat (up to 30s) would let this
    // runner claim pending jobs that then block the graceful exit.
    let (frozen, restart_requested): (bool, bool) =
        sqlx::query_as("SELECT is_frozen != 0, restart_requested != 0 FROM runners WHERE id = ?")
            .bind(&auth.runner_id)
            .fetch_one(&state.pool)
            .await
            .map(|(f, r): (i64, i64)| (f != 0, r != 0))
            .unwrap_or((false, false));

    if frozen {
        tracing::debug!("Runner {} is frozen — skipping poll", auth.runner_id);
        return Ok(Json(ApiResponse::success(Some(serde_json::json!({
            "cancelled_jobs": cancelled_jobs,
            "retry_after_ms": 2000,
        })))));
    }
    if restart_requested {
        tracing::debug!(
            "Runner {} has a pending restart — skipping poll",
            auth.runner_id
        );
        return Ok(Json(ApiResponse::success(Some(serde_json::json!({
            "cancelled_jobs": cancelled_jobs,
            "retry_after_ms": 2000,
        })))));
    }

    // Capacity gate: do not claim when this runner is already at max concurrent jobs.
    let max_concurrent: i64 =
        sqlx::query_scalar("SELECT max_concurrent_jobs FROM runners WHERE id = ?")
            .bind(&auth.runner_id)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(1)
            .max(1);

    let running_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE runner_id = ? AND status = 'running'")
            .bind(&auth.runner_id)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);

    if running_count >= max_concurrent || active_jobs >= max_concurrent {
        tracing::debug!(
            runner = %auth.runner_id,
            running = running_count,
            max = max_concurrent,
            "Runner at capacity — skipping poll"
        );
        return Ok(Json(ApiResponse::success(Some(cancel_only_response(
            cancelled_jobs,
        )))));
    }

    // Runner team binding is a dispatch boundary. NULL means explicitly shared.
    let runner_team_id: Option<String> =
        sqlx::query_scalar("SELECT team_id FROM runners WHERE id = ?")
            .bind(&auth.runner_id)
            .fetch_one(&state.pool)
            .await?;

    // Load this runner's labels for scheduling.
    let runner_labels: Vec<String> =
        sqlx::query_scalar::<_, String>("SELECT labels FROM runners WHERE id = ?")
            .bind(&auth.runner_id)
            .fetch_optional(&state.pool)
            .await?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();

    // Slot-aware dispatch context: execution mode, attached sandbox host,
    // queue-walk strategy, and the staleness threshold (see slot_scheduler).
    let dispatch = {
        let row: (String, String, String, String) = sqlx::query_as(
            "SELECT execution_mode, host_id, sandbox_instance_id, claim_strategy
             FROM runners WHERE id = ?",
        )
        .bind(&auth.runner_id)
        .fetch_one(&state.pool)
        .await?;
        let stale_after_seconds: i64 = crate::services::platform_settings::effective(
            &state.pool,
            crate::services::platform_settings::KEY_SANDBOX_CONFIG_STALE_SECONDS,
            &crate::services::platform_settings::DEFAULT_SANDBOX_CONFIG_STALE_SECONDS.to_string(),
        )
        .await
        .trim()
        .parse()
        .unwrap_or(crate::services::platform_settings::DEFAULT_SANDBOX_CONFIG_STALE_SECONDS as i64);
        crate::services::slot_scheduler::RunnerDispatchContext {
            execution_mode: parse_execution_mode(row.0)?,
            host_id: row.1,
            sandbox_instance_id: row.2,
            strategy: crate::services::slot_scheduler::ClaimStrategy::parse(&row.3)
                .unwrap_or_default(),
            stale_after_seconds,
        }
    };

    // Aging: past this wait a backfill runner stops overtaking the job
    // (0 disables). Read per poll, next to staleness — same cadence, same
    // settings source.
    let starvation_seconds: i64 = crate::services::platform_settings::effective(
        &state.pool,
        crate::services::platform_settings::KEY_SCHEDULER_STARVATION_SECONDS,
        &crate::services::platform_settings::DEFAULT_SCHEDULER_STARVATION_SECONDS.to_string(),
    )
    .await
    .trim()
    .parse()
    .unwrap_or(crate::services::platform_settings::DEFAULT_SCHEDULER_STARVATION_SECONDS as i64);

    // Candidates: oldest pending jobs with pipeline (+ snapshot) labels.
    // Claim only if runner has ALL required labels (see runner_satisfies_labels).
    let candidates = sqlx::query(
        "SELECT j.id, j.pipeline_id, j.trigger_params, COALESCE(j.snapshot, '{}'), p.runner_labels,
                j.debug_unredact, j.verbose_logs, j.trigger_by, p.name,
                COALESCE(json_extract(j.snapshot, '$.team_id'), p.team_id), p.trigger_type,
                j.environment_id, j.run_number, j.run_attempt,
                CAST(strftime('%s', 'now') - strftime('%s', j.created_at) AS INTEGER) AS waited_seconds
         FROM jobs j
         JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.status = 'pending'
           AND (? IS NULL OR json_extract(j.snapshot, '$.team_id') = ?)
           AND (
             j.environment_id IS NULL
             OR COALESCE((SELECT deployment_concurrency FROM environments e WHERE e.id = j.environment_id), 'off') != 'cancel_in_progress'
             OR j.rowid = (
                 SELECT MAX(latest.rowid) FROM jobs latest
                 WHERE latest.environment_id = j.environment_id AND latest.status = 'pending'
             )
           )
           AND NOT (
             COALESCE((SELECT deployment_concurrency FROM environments e WHERE e.id = j.environment_id), 'off') = 'queue'
             AND EXISTS (SELECT 1 FROM jobs running WHERE running.environment_id = j.environment_id AND running.status = 'running')
           )
           AND NOT (
             COALESCE((SELECT deployment_concurrency FROM environments e WHERE e.id = j.environment_id), 'off') != 'cancel_in_progress'
             AND EXISTS (SELECT 1 FROM jobs running WHERE running.pipeline_id = j.pipeline_id AND running.environment_id = j.environment_id AND running.status = 'running')
           )
         ORDER BY j.created_at ASC, j.rowid ASC
         LIMIT 50",
    )
    .bind(&runner_team_id)
    .bind(&runner_team_id)
    .fetch_all(&state.pool)
    .await?;

    let mut claimed = None;
    for row in candidates {
        let job_id: String = row.get(0);
        let snapshot_raw: String = row.get(3);
        let pipeline_labels_json: String = row.get(4);
        let debug_unredact: bool = row.get::<i64, _>(5) != 0;
        let verbose_logs: bool = row.get::<i64, _>(6) != 0;
        let required = required_labels_for_job(&snapshot_raw, &pipeline_labels_json);
        if !runner_satisfies_labels(&runner_labels, &required) {
            continue;
        }

        // Re-check the resource-profile grant at claim: it may have been
        // revoked while the job was queued. Revocation fails the job
        // terminally rather than leaving it pending forever — and it is a
        // different failure from "no host offers this profile" (which queues).
        let (job_profile, job_team) = job_resource_request(&snapshot_raw);
        if let Some(profile) = crate::services::resource_grants::requested_profile(&job_profile) {
            if let Err(error) = crate::services::resource_grants::ensure_team_may_use(
                &state.pool,
                job_team.as_deref().unwrap_or_default(),
                profile,
            )
            .await
            {
                tracing::warn!(
                    job = %job_id,
                    profile,
                    team = ?job_team,
                    %error,
                    "resource profile grant revoked while queued; failing job"
                );
                let _ = sqlx::query(
                    "UPDATE jobs SET status = 'failed', end_time = datetime('now'),
                     updated_at = datetime('now') WHERE id = ? AND status = 'pending'",
                )
                .bind(&job_id)
                .execute(&state.pool)
                .await;
                crate::services::notify::spawn_job_notify(
                    state.pool.clone(),
                    state.config.auth.jwt_secret.clone(),
                    state.config.server.public_base_url.clone(),
                    job_id.clone(),
                    "failed".to_string(),
                );
                continue;
            }
        }

        // Serialize environment gate evaluation with the claim. SQLite's
        // immediate transaction reserves the writer before checking blockers,
        // so another poller must re-evaluate after this transaction commits.
        let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
        let candidate = sqlx::query(
            "SELECT j.pipeline_id, j.environment_id, r.team_id, \
                    json_extract(j.snapshot, '$.team_id') \
             FROM jobs j JOIN runners r ON r.id = ? \
             WHERE j.id = ? AND j.status = 'pending'",
        )
        .bind(&auth.runner_id)
        .bind(&job_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(candidate) = candidate else {
            tx.rollback().await?;
            continue;
        };
        let candidate_pipeline_id: String = candidate.get(0);
        let candidate_environment_id: Option<String> = candidate.get(1);
        let candidate_runner_team: Option<String> = candidate.get(2);
        let candidate_job_team: Option<String> = candidate.get(3);
        if candidate_runner_team.is_some() && candidate_runner_team != candidate_job_team {
            tx.rollback().await?;
            continue;
        }

        // Slot gate — inside the IMMEDIATE transaction so two concurrent
        // polls cannot both see "fits" and oversell the host budget.
        let (slots, resource_spec, sandbox_key) = match crate::services::slot_scheduler::decide(
            &mut tx,
            &dispatch,
            &job_profile,
        )
        .await?
        {
            crate::services::slot_scheduler::Decision::Dispatch {
                slots,
                spec,
                sandbox_key,
            } => (slots, spec, sandbox_key),
            crate::services::slot_scheduler::Decision::WaitForCapacity { reason } => {
                tx.rollback().await?;
                record_dispatch_reason(&state.pool, &job_id, &reason).await;
                match dispatch.strategy {
                    // Strict order within this Runner's eligible set: the
                    // oldest fitting-in-principle job blocks the round.
                    crate::services::slot_scheduler::ClaimStrategy::Fifo => break,
                    crate::services::slot_scheduler::ClaimStrategy::Backfill => {
                        // Aging: once a job has waited past the starvation
                        // threshold, stop letting later candidates overtake
                        // it — end the round so the next freed capacity goes
                        // to it instead of another small job.
                        let waited: i64 = row.get("waited_seconds");
                        if starvation_seconds > 0 && waited >= starvation_seconds {
                            tracing::info!(
                                job = %job_id,
                                waited_seconds = waited,
                                threshold = starvation_seconds,
                                "backfill suspended: job passed the starvation threshold"
                            );
                            break;
                        }
                        continue;
                    }
                }
            }
            crate::services::slot_scheduler::Decision::NotEligible { reason } => {
                tx.rollback().await?;
                record_dispatch_reason(&state.pool, &job_id, &reason).await;
                continue;
            }
        };
        let mut superseded_ids: Vec<String> = Vec::new();

        if let Some(env_id) = candidate_environment_id.as_deref() {
            let deployment_concurrency: String = sqlx::query_scalar(
                "SELECT COALESCE(deployment_concurrency, 'off') FROM environments WHERE id = ?",
            )
            .bind(env_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or_else(|| "off".to_string());

            if deployment_concurrency == "cancel_in_progress" {
                let newest_pending: Option<String> = sqlx::query_scalar(
                    "SELECT id FROM jobs WHERE environment_id = ? AND status = 'pending' \
                     ORDER BY rowid DESC LIMIT 1",
                )
                .bind(env_id)
                .fetch_optional(&mut *tx)
                .await?;
                if newest_pending.as_deref() != Some(job_id.as_str()) {
                    tx.rollback().await?;
                    continue;
                }

                let superseded_rows = sqlx::query(
                    "SELECT id, status, step_statuses, COALESCE(snapshot, '{}') \
                     FROM jobs WHERE environment_id = ? AND id != ? \
                     AND status IN ('pending', 'queued', 'waiting_approval', 'running')",
                )
                .bind(env_id)
                .bind(&job_id)
                .fetch_all(&mut *tx)
                .await?;
                for superseded in superseded_rows {
                    let superseded_id: String = superseded.get(0);
                    let old_status: String = superseded.get(1);
                    let step_statuses_raw: String = superseded.get(2);
                    let snapshot_raw: String = superseded.get(3);
                    let mut step_statuses: Vec<serde_json::Value> =
                        serde_json::from_str(&step_statuses_raw).unwrap_or_default();
                    for step in &mut step_statuses {
                        if step.get("status").and_then(serde_json::Value::as_str) == Some("running")
                        {
                            step["status"] = serde_json::json!("cancelled");
                            step["finished_at"] =
                                serde_json::json!(chrono::Utc::now().to_rfc3339());
                        }
                    }
                    let mut snapshot: serde_json::Value =
                        serde_json::from_str(&snapshot_raw).unwrap_or(serde_json::json!({}));
                    if old_status == "waiting_approval" {
                        let received = snapshot
                            .get("approval")
                            .and_then(|approval| approval.get("received"))
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or(0);
                        if let Some(object) = snapshot.as_object_mut() {
                            object.insert(
                                "approval".into(),
                                serde_json::json!({
                                    "required": true,
                                    "received": received,
                                    "outcome": "cancelled",
                                }),
                            );
                        }
                    }
                    let updated = sqlx::query(
                        "UPDATE jobs SET status = 'cancelled', step_statuses = ?, snapshot = ?, \
                         cancel_ack_pending = ?, \
                         end_time = datetime('now'), updated_at = datetime('now') \
                         WHERE id = ? AND status IN \
                         ('pending', 'queued', 'waiting_approval', 'running')",
                    )
                    .bind(serde_json::to_string(&step_statuses).unwrap_or_else(|_| "[]".into()))
                    .bind(serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into()))
                    .bind(i64::from(old_status == "running"))
                    .bind(&superseded_id)
                    .execute(&mut *tx)
                    .await?;
                    if updated.rows_affected() > 0 {
                        if old_status == "waiting_approval" {
                            sqlx::query(
                                "INSERT OR IGNORE INTO job_approvals \
                                 (id, job_id, user_id, username, action, comment) \
                                 VALUES (?, ?, 'system:environment', 'environment concurrency', \
                                         'cancel', 'superseded by cancel_in_progress')",
                            )
                            .bind(Uuid::new_v4().to_string())
                            .bind(&superseded_id)
                            .execute(&mut *tx)
                            .await?;
                        }
                        superseded_ids.push(superseded_id);
                    }
                }
            } else {
                let pair_running: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM jobs \
                     WHERE pipeline_id = ? AND environment_id = ? AND status = 'running' AND id != ?",
                )
                .bind(&candidate_pipeline_id)
                .bind(env_id)
                .bind(&job_id)
                .fetch_one(&mut *tx)
                .await?;
                if pair_running > 0 {
                    tx.rollback().await?;
                    continue;
                }

                if deployment_concurrency == "queue" {
                    let env_running: i64 = sqlx::query_scalar(
                        "SELECT COUNT(*) FROM jobs WHERE environment_id = ? AND status = 'running'",
                    )
                    .bind(env_id)
                    .fetch_one(&mut *tx)
                    .await?;
                    if env_running > 0 {
                        tx.rollback().await?;
                        continue;
                    }
                }
            }
        }

        let got = sqlx::query(
            "UPDATE jobs SET status = 'running', runner_id = ?, start_time = datetime('now'),
             resource_slots = ?, resource_spec = ?, sandbox_instance_id = ?, dispatch_reason = '',
             updated_at = datetime('now')
             WHERE id = ? AND status = 'pending'
             RETURNING id, pipeline_id, trigger_params, environment_id, run_number, run_attempt",
        )
        .bind(&auth.runner_id)
        .bind(slots)
        .bind(resource_spec.to_string())
        .bind(&sandbox_key)
        .bind(&job_id)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(r) = got {
            tx.commit().await?;
            if !superseded_ids.is_empty() {
                crate::services::poll_events::wake_all();
            }
            for superseded_id in &superseded_ids {
                crate::services::transition::announce_job_status(
                    &state,
                    superseded_id,
                    "cancelled",
                    true,
                )
                .await;
                crate::audit::record(
                    &state.pool,
                    crate::audit::AuditEvent::system(Action::EnvironmentConcurrencyCancel)
                        .resource("job", superseded_id, "superseded by cancel_in_progress"),
                )
                .await;
            }
            if !superseded_ids.is_empty() {
                crate::services::poll_events::wake_all();
                tracing::info!(
                    environment = ?candidate_environment_id,
                    superseded = superseded_ids.len(),
                    "cancel_in_progress superseded in-flight deployments"
                );
            }
            tracing::info!(
                runner = %auth.runner_id,
                job = %job_id,
                runner_labels = ?runner_labels,
                required_labels = ?required,
                debug_unredact,
                verbose_logs,
                "Runner claimed job"
            );
            claimed = Some((
                r,
                snapshot_raw,
                debug_unredact,
                verbose_logs,
                row.get::<String, _>(7),
                row.get::<String, _>(8),
                row.get::<Option<String>, _>(9),
                row.get::<String, _>(10),
            ));
            break;
        }
        tx.rollback().await?;
        // Lost the race — try next matching candidate
    }

    let Some((
        row,
        snapshot_raw,
        debug_unredact,
        verbose_logs,
        trigger_by,
        pipeline_name,
        team_id,
        pipeline_trigger_type,
    )) = claimed
    else {
        // No matching pending job — still a delivery point for cancels.
        return Ok(Json(ApiResponse::success(Some(cancel_only_response(
            cancelled_jobs,
        )))));
    };

    build_claim_payload(
        &state,
        &auth,
        ClaimedJob {
            job_id: row.get(0),
            pipeline_id: row.get(1),
            trigger_params_raw: row.get(2),
            job_environment_id: row.get(3),
            run_number: row.get(4),
            run_attempt: row.get(5),
            snapshot_raw,
            debug_unredact,
            verbose_logs,
            trigger_by,
            pipeline_name,
            team_id,
            pipeline_trigger_type,
            cancelled_jobs,
        },
    )
    .await
    .map(|payload| Json(ApiResponse::success(Some(payload))))
}

/// One claimed job's row values, handed to [`build_claim_payload`].
struct ClaimedJob {
    job_id: String,
    pipeline_id: String,
    trigger_params_raw: Option<String>,
    job_environment_id: Option<String>,
    run_number: i64,
    run_attempt: i64,
    snapshot_raw: String,
    debug_unredact: bool,
    verbose_logs: bool,
    trigger_by: String,
    pipeline_name: String,
    team_id: Option<String>,
    pipeline_trigger_type: String,
    cancelled_jobs: Vec<String>,
}

/// Post-claim payload assembly: decrypt trigger params, resolve secrets /
/// environment overrides / step env / SSH hosts / marketplace inputs, and
/// return the JSON body the Runner executes.
async fn build_claim_payload(
    state: &AppState,
    auth: &RunnerClaims,
    claimed: ClaimedJob,
) -> Result<serde_json::Value, AppError> {
    let ClaimedJob {
        job_id,
        pipeline_id,
        trigger_params_raw,
        job_environment_id,
        run_number,
        run_attempt,
        snapshot_raw,
        debug_unredact,
        verbose_logs,
        trigger_by,
        pipeline_name,
        team_id,
        pipeline_trigger_type,
        cancelled_jobs,
    } = claimed;

    let trigger_params_stored: serde_json::Value = trigger_params_raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}));

    let job_snapshot: serde_json::Value =
        serde_json::from_str(&snapshot_raw).unwrap_or(serde_json::json!({}));
    let mut masking_input_keys: Vec<String> = job_snapshot
        .get("masking_input_keys")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let trigger_params_map = crypto::trigger_params_to_map(&trigger_params_stored);

    // Keys sealed at trigger time (buzenc:v1:) are authoritative even if snapshot list is stale.
    for (k, v) in &trigger_params_map {
        if v.starts_with(crypto::TRIGGER_PARAM_ENC_PREFIX) && !masking_input_keys.contains(k) {
            masking_input_keys.push(k.clone());
        }
    }

    // Fallback for jobs created before masking_input_keys existed: use live pipeline defs.
    if masking_input_keys.is_empty() {
        let defs_raw: String = sqlx::query_scalar("SELECT input_defs FROM pipelines WHERE id = ?")
            .bind(&pipeline_id)
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or_else(|| "[]".into());
        let defs: Vec<shared::protocol::PipelineInputDef> =
            serde_json::from_str(&defs_raw).unwrap_or_default();
        masking_input_keys = shared::protocol::masking_input_names(&defs);
    }

    let trigger_params_plain =
        crypto::open_trigger_params(&trigger_params_map, &state.config.auth.jwt_secret).map_err(
            |e| {
                tracing::error!(job = %job_id, error = %e, "Failed to decrypt trigger_params");
                AppError::Internal(e)
            },
        )?;
    let trigger_params = serde_json::to_value(&trigger_params_plain).unwrap_or_default();
    let has_frozen_tasks = job_snapshot
        .get("tasks")
        .and_then(|tasks| tasks.as_array())
        .is_some();
    let marketplace_by_order: std::collections::HashMap<i64, serde_json::Value> = job_snapshot
        .get("tasks")
        .and_then(|tasks| tasks.as_array())
        .into_iter()
        .flatten()
        .filter_map(|task| {
            let order = task.get("step_order")?.as_i64()?;
            let marketplace = task.get("marketplace")?.clone();
            Some((order, marketplace))
        })
        .collect();
    let mut resolutions_by_order: std::collections::HashMap<i64, String> = marketplace_by_order
        .iter()
        .filter_map(|(order, marketplace)| {
            serde_json::to_string(marketplace.get("inputs")?)
                .ok()
                .map(|v| (*order, v))
        })
        .collect();
    if !has_frozen_tasks {
        resolutions_by_order = sqlx::query_as(
            "SELECT step_order, input_defs_json FROM step_resolutions WHERE pipeline_id = ?",
        )
        .bind(&pipeline_id)
        .fetch_all(&state.pool)
        .await?
        .into_iter()
        .collect();
    }
    let task_rows = if has_frozen_tasks {
        sqlx::query(
            "SELECT COALESCE(json_extract(j.value, '$.id'), ''), json_extract(j.value, '$.step_order'),
                    json_extract(j.value, '$.name'), json_extract(j.value, '$.exec_type'),
                    COALESCE(json_extract(j.value, '$.host_id'), ''), COALESCE(json_extract(j.value, '$.host_names'), ''),
                    COALESCE(json_extract(j.value, '$.host_labels'), ''),
                    COALESCE(json_extract(j.value, '$.command_template'), ''),
                    COALESCE(json_extract(j.value, '$.work_dir'), ''),
                    COALESCE(json_extract(j.value, '$.retry_count'), 0),
                    COALESCE(json_extract(j.value, '$.timeout_seconds'), 0),
                    COALESCE(json_extract(j.value, '$.fail_strategy'), 'stop'),
                    COALESCE(json_extract(j.value, '$.step_secrets'), ''),
                    COALESCE(json_extract(j.value, '$.step_env'), '{}'),
                    COALESCE(json_extract(j.value, '$.image'), ''),
                    COALESCE(json_extract(j.value, '$.volumes'), '[]'),
                    COALESCE(json_extract(j.value, '$.hosts_strategy'), 'chain'),
                    COALESCE(json_extract(j.value, '$.privileged'), 0),
                    COALESCE(json_extract(j.value, '$.capabilities'), '[]'),
                    COALESCE(json_extract(j.value, '$.hosts_max_parallel'), NULL),
                    COALESCE(json_extract(j.value, '$.needs'), '[]')
             FROM json_each(?, '$.tasks') j
             ORDER BY json_extract(j.value, '$.step_order')",
        )
        .bind(&snapshot_raw)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query(
            "SELECT id, step_order, name, exec_type, COALESCE(host_id, ''), host_names, host_labels, command_template,
                    work_dir, retry_count, timeout_seconds, fail_strategy, step_secrets, step_env, image, volumes,
                    hosts_strategy,
                    privileged, capabilities, hosts_max_parallel, needs_json
             FROM tasks WHERE pipeline_id = ? ORDER BY step_order",
        )
        .bind(&pipeline_id)
        .fetch_all(&state.pool)
        .await?
    };

    // Resolve pipeline secrets from explicit bindings + team_shared (sensitive).
    // `secrets:` / team_shared are an **allowlist** for `${secrets.NAME}` —
    // values are NOT auto-injected as `$NAME`. Call sites must map explicitly:
    //   env:
    //     GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}
    //
    // Claim still sends `secrets` (name→value) for log masking + config.render.
    let mut secrets: serde_json::Value = serde_json::json!({});
    let mut secrets_by_name: serde_json::Value = serde_json::json!({});
    let mut env: serde_json::Value = serde_json::json!({});
    let mut secret_env_keys: Vec<String> = Vec::new();

    // Environment Secret overrides (name → plaintext). Applied only when the
    // secret is already on the job allowlist (bound / team_shared / implied).
    let env_secret_overrides: std::collections::HashMap<String, String> =
        if let Some(ref env_id) = job_environment_id {
            super::environments::load_secret_override_plaintexts(
                &state.pool,
                env_id,
                &state.config.auth.jwt_secret,
            )
            .await?
        } else {
            std::collections::HashMap::new()
        };

    // ── Explicit secret bindings (pipeline_secrets table) ──
    // Same-team only: ignore stale/cross-team rows if any slipped into the table.
    let explicit_rows = sqlx::query(
        "SELECT s.name, s.value
         FROM pipeline_secrets ps
         JOIN secrets s ON s.id = ps.secret_id
         JOIN pipelines p ON p.id = ps.pipeline_id AND s.team_id = p.team_id AND p.team_id != ''
         WHERE ps.pipeline_id = ?",
    )
    .bind(&pipeline_id)
    .fetch_all(&state.pool)
    .await?;
    for sr in &explicit_rows {
        let name: String = sr.get(0);
        let stored: String = sr.get(1);
        match crypto::decrypt(&stored, &state.config.auth.jwt_secret) {
            Ok(decrypted) => {
                let value = env_secret_overrides
                    .get(&name)
                    .cloned()
                    .unwrap_or(decrypted);
                if let Some(obj) = secrets.as_object_mut() {
                    obj.insert(name.clone(), serde_json::Value::String(value.clone()));
                }
                if let Some(obj) = secrets_by_name.as_object_mut() {
                    obj.insert(name.clone(), serde_json::Value::String(value));
                }
            }
            Err(e) => {
                tracing::error!("Failed to decrypt pipeline secret '{name}': {e}");
            }
        }
    }

    // ── Team auto-access: same team_id AND team_shared opt-in ──
    let team_secret_rows = sqlx::query(
        "SELECT s.name, s.value
         FROM secrets s
         JOIN pipelines p ON p.team_id = s.team_id AND p.team_id != ''
         WHERE p.id = ? AND s.team_shared = 1",
    )
    .bind(&pipeline_id)
    .fetch_all(&state.pool)
    .await?;
    for sr in &team_secret_rows {
        let name: String = sr.get(0);
        let stored: String = sr.get(1);
        match crypto::decrypt(&stored, &state.config.auth.jwt_secret) {
            Ok(decrypted) => {
                let value = env_secret_overrides
                    .get(&name)
                    .cloned()
                    .unwrap_or(decrypted);
                if let Some(obj) = secrets.as_object_mut() {
                    obj.entry(name.clone())
                        .or_insert(serde_json::Value::String(value.clone()));
                }
                if let Some(obj) = secrets_by_name.as_object_mut() {
                    obj.entry(name.clone())
                        .or_insert(serde_json::Value::String(value));
                }
            }
            Err(e) => {
                tracing::error!("Failed to decrypt team secret '{name}': {e}");
            }
        }
    }

    if !env_secret_overrides.is_empty() {
        let applied = secrets_by_name
            .as_object()
            .map(|o| {
                env_secret_overrides
                    .keys()
                    .filter(|k| o.contains_key(k.as_str()))
                    .count()
            })
            .unwrap_or(0);
        tracing::info!(
            job = %job_id,
            environment_id = job_environment_id.as_deref().unwrap_or(""),
            applied,
            "claim: applied environment secret overrides"
        );
    }

    // ── Explicit variable bindings (pipeline_vars table) — not secrets ──
    let var_rows = sqlx::query(
        "SELECT pv.name, pv.value
         FROM pipeline_vars pvars
         JOIN platform_vars pv ON pv.id = pvars.var_id
         WHERE pvars.pipeline_id = ?",
    )
    .bind(&pipeline_id)
    .fetch_all(&state.pool)
    .await?;
    for vr in &var_rows {
        let name: String = vr.get(0);
        let value: String = vr.get(1);
        if let Some(obj) = env.as_object_mut() {
            obj.insert(name, serde_json::Value::String(value));
        }
    }

    // Environment Variable overrides (same keys only — does not inject unbound vars).
    if let Some(ref env_id) = job_environment_id {
        let override_rows = sqlx::query(
            "SELECT pv.name, o.value
             FROM environment_var_overrides o
             JOIN platform_vars pv ON pv.id = o.var_id
             WHERE o.environment_id = ?",
        )
        .bind(env_id)
        .fetch_all(&state.pool)
        .await?;
        let mut applied = 0u32;
        for orow in &override_rows {
            let name: String = orow.get(0);
            let value: String = orow.get(1);
            if let Some(obj) = env.as_object_mut() {
                if obj.contains_key(&name) {
                    obj.insert(name, serde_json::Value::String(value));
                    applied += 1;
                }
            }
        }
        tracing::info!(
            job = %job_id,
            environment_id = %env_id,
            applied,
            "claim: applied environment variable overrides"
        );
    }

    // ── Inline env vars (user-defined per pipeline, from `env:` YAML block or form) ──
    let env_rows = sqlx::query(
        "SELECT var_name, value FROM variables
         WHERE pipeline_id = ? AND source_type = 'inline' AND is_secret = 0",
    )
    .bind(&pipeline_id)
    .fetch_all(&state.pool)
    .await?;
    for er in &env_rows {
        let name: String = er.get(0);
        let value: String = er.get(1);
        if let Some(obj) = env.as_object_mut() {
            obj.insert(name, serde_json::Value::String(value));
        }
    }

    // Implicit allowlist: `${secrets.X}` in pipeline env loads same-team secrets
    // even when top-level `secrets:` was omitted (GHA-style).
    {
        let mut needed = Vec::new();
        if let Some(obj) = env.as_object() {
            for v in obj.values() {
                if let Some(s) = v.as_str() {
                    needed.extend(shared::secret_ref::collect_secrets_names(s));
                }
            }
        }
        merge_team_secrets_by_name(
            &state.pool,
            &pipeline_id,
            &state.config.auth.jwt_secret,
            &needed,
            &mut secrets_by_name,
            Some(&mut secrets),
            &env_secret_overrides,
        )
        .await?;
    }

    // Expand pipeline `env:` values (`GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}` → concrete).
    {
        let value_map = claim_value_map(&secrets_by_name, &env, &trigger_params);
        if let Some(obj) = env.as_object_mut() {
            for (key, val) in obj.iter_mut() {
                let Some(raw) = val.as_str() else { continue };
                if raw.contains("${secrets.") {
                    secret_env_keys.push(key.clone());
                }
                match shared::value_source::expand_env_value(raw, &value_map) {
                    Ok(expanded) => *val = serde_json::Value::String(expanded),
                    Err(e) => {
                        return Err(AppError::ValidationFailed(format!(
                            "pipeline env '{key}': {e}"
                        )));
                    }
                }
            }
        }
    }

    let mut tasks: Vec<serde_json::Value> = Vec::new();
    for t in &task_rows {
        let exec_type: String = t.get(3);
        let host_id_str: String = t.get(4);
        let host_names_raw: String = t.get(5);
        let host_labels_raw: String = t.get(6);
        let step_secrets_str: String = t.get(12);
        let step_env_raw: String = t.get(13);
        let image: String = t.get(14);
        let volumes_raw: String = t.get(15);
        let hosts_strategy: String = t.get(16);
        let privileged: i64 = t.get(17);
        let capabilities_raw: String = t.get(18);
        let needs_raw: String = t.get(20);
        let hosts_max_parallel: Option<i64> = t.get(19);
        let capabilities: Vec<String> = serde_json::from_str(&capabilities_raw).map_err(|_| {
            AppError::ValidationFailed(format!(
                "task {} has invalid capabilities JSON",
                t.get::<String, _>(0)
            ))
        })?;
        shared::plugin_spec::validate_plugin_capabilities(&capabilities, &exec_type)
            .map_err(AppError::ValidationFailed)?;

        let mut task_json = serde_json::json!({
            "id": t.get::<String, _>(0),
            "step_order": t.get::<i64, _>(1),
            "name": t.get::<String, _>(2),
            "exec_type": &exec_type,
            "command_template": t.get::<String, _>(7),
            "work_dir": t.get::<String, _>(8),
            "retry_count": t.get::<i64, _>(9),
            "timeout_seconds": t.get::<i64, _>(10),
            "fail_strategy": t.get::<String, _>(11),
            "image": &image,
            "volumes": serde_json::from_str::<serde_json::Value>(&volumes_raw)
                .unwrap_or(serde_json::Value::Array(vec![])),
            "hosts_strategy": shared::pipeline_yaml::normalize_hosts_strategy(
                Some(hosts_strategy.as_str()),
            )
            .unwrap_or("chain"),
            "privileged": privileged != 0,
            "capabilities": capabilities,
            "hosts_max_parallel": hosts_max_parallel.map(|v| v as u64),
            "needs": serde_json::from_str::<Vec<String>>(&needs_raw)
                .unwrap_or_default(),
        });

        // Values that must be redacted in the Runner's logs. Registered here — the
        // single source of truth for "which values are sensitive" (see
        // `shared::log_masking::SecretClass`). Adding a new sensitive field means
        // pushing its value here; the Runner only consumes this list.
        //
        // Both former contributors (registry passwords, OSS signed URLs) were
        // removed with their kernel paths in the 2.x retirement; the list stays as the hook for
        // plugin-injected sensitive values.
        let log_mask_values: Vec<String> = Vec::new();

        if let Some(defs_raw) = resolutions_by_order.get(&t.get::<i64, _>(1)) {
            let defs: Vec<shared::protocol::PipelineInputDef> =
                serde_json::from_str(defs_raw).unwrap_or_default();
            task_json["marketplace_input_defs"] = serde_json::to_value(&defs).unwrap_or_default();
            let masking_keys: Vec<String> = defs
                .iter()
                .filter(|def| def.is_masking())
                .map(|def| def.name.clone())
                .collect();
            task_json["masking_input_keys"] =
                serde_json::to_value(masking_keys).unwrap_or_default();
        }

        // Resolve step allowlist secrets (values for ${secrets.X} / render / mask — not env inject).
        let mut step_secrets_map = serde_json::json!({});
        if !step_secrets_str.is_empty() {
            let step_names: Vec<String> = step_secrets_str
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            merge_team_secrets_by_name(
                &state.pool,
                &pipeline_id,
                &state.config.auth.jwt_secret,
                &step_names,
                &mut step_secrets_map,
                None,
                &env_secret_overrides,
            )
            .await?;
        }

        // Implicit allowlist from purpose slots + step env `${secrets.X}` (top-level optional).
        {
            let mut implied: Vec<String> = Vec::new();
            if !step_env_raw.trim().is_empty() {
                if let Ok(raw_map) = serde_json::from_str::<
                    std::collections::BTreeMap<String, String>,
                >(&step_env_raw)
                {
                    for v in raw_map.values() {
                        implied.extend(shared::secret_ref::collect_secrets_names(v));
                    }
                }
            }
            implied.sort();
            implied.dedup();
            // Prefer job-level map when already bound; otherwise load into step map.
            let mut still_needed = Vec::new();
            for n in implied {
                if secrets_by_name.get(&n).and_then(|v| v.as_str()).is_none() {
                    still_needed.push(n);
                }
            }
            merge_team_secrets_by_name(
                &state.pool,
                &pipeline_id,
                &state.config.auth.jwt_secret,
                &still_needed,
                &mut step_secrets_map,
                None,
                &env_secret_overrides,
            )
            .await?;
        }

        // Expand step `env:` map (required path for `$NAME` in run scripts).
        let mut step_env_expanded = serde_json::json!({});
        let mut step_secret_env_keys: Vec<String> = Vec::new();
        if !step_env_raw.trim().is_empty() {
            if let Ok(raw_map) =
                serde_json::from_str::<std::collections::BTreeMap<String, String>>(&step_env_raw)
            {
                let mut combined_secrets = secrets_by_name.clone();
                if let (Some(dst), Some(src)) = (
                    combined_secrets.as_object_mut(),
                    step_secrets_map.as_object(),
                ) {
                    for (k, v) in src {
                        dst.entry(k.clone()).or_insert(v.clone());
                    }
                }
                let value_map = claim_value_map(&combined_secrets, &env, &trigger_params);
                for (key, raw) in raw_map {
                    if raw.contains("${secrets.") {
                        step_secret_env_keys.push(key.clone());
                    }
                    match shared::value_source::expand_env_value(&raw, &value_map) {
                        Ok(expanded) => {
                            if let Some(obj) = step_env_expanded.as_object_mut() {
                                obj.insert(key, serde_json::Value::String(expanded));
                            }
                        }
                        Err(e) => {
                            task_json["env_expand_error"] =
                                serde_json::Value::String(format!("step env '{key}': {e}"));
                            break;
                        }
                    }
                }
            } else {
                task_json["env_expand_error"] =
                    serde_json::Value::String("step env is not a valid JSON object map".into());
            }
        }
        task_json["step_env"] = step_env_expanded;
        task_json["secret_env_keys"] = serde_json::Value::Array(
            step_secret_env_keys
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        );
        if let Some(defs_raw) = resolutions_by_order.get(&t.get::<i64, _>(1)) {
            let defs: Vec<shared::protocol::PipelineInputDef> =
                serde_json::from_str(defs_raw).unwrap_or_default();
            let mut input_values = std::collections::HashMap::new();
            for def in &defs {
                if let Some(value) =
                    task_json["step_env"][shared::protocol::input_env_key(&def.name)].as_str()
                {
                    // `${env.X}` values resolve on the Runner at execution time
                    // (previous steps' $BUZ_ENV) — skip literal type checks.
                    if value.contains("${env.") {
                        continue;
                    }
                    input_values.insert(def.name.clone(), value.to_string());
                }
            }
            let validation_defs: Vec<_> = defs
                .iter()
                .cloned()
                .map(|mut def| {
                    def.required = Some(false);
                    def.default = None;
                    def
                })
                .collect();
            if let Err(field_errors) =
                shared::protocol::validate_input_values(&validation_defs, &input_values, |_| false)
            {
                task_json["input_validation_error"] = serde_json::Value::String(
                    field_errors
                        .iter()
                        .map(|error| format!("{}: {}", error.name, error.message))
                        .collect::<Vec<_>>()
                        .join("; "),
                );
            }
            let keys: Vec<String> = defs
                .iter()
                .filter(|def| def.is_masking())
                .map(|def| shared::protocol::input_env_key(&def.name))
                .collect();
            task_json["masking_env_keys"] = serde_json::to_value(keys).unwrap_or_default();
        }

        // For SSH steps, resolve host IDs → connection details
        if exec_type == "ssh" {
            let mut host_ids: Vec<String> = host_id_str
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            // ── Dynamic host resolution: resolve variable references ──
            // Use expanded pipeline env + trigger inputs only (secrets are not auto `$NAME`).
            let mut unified_vars = serde_json::json!({});
            if let Some(obj) = env.as_object() {
                for (k, v) in obj {
                    unified_vars[k] = v.clone();
                }
            }
            if let Some(obj) = trigger_params.as_object() {
                for (k, v) in obj {
                    unified_vars[k] = v.clone();
                }
            }

            if host_ids.is_empty()
                || host_names_raw.chars().any(|c| c == '$')
                || host_labels_raw.chars().any(|c| c == '$')
            {
                let resolved_names: Vec<String> = host_names_raw
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .flat_map(|s| resolve_var_ref(s.trim(), &unified_vars))
                    .collect();

                let resolved_labels: Vec<String> = host_labels_raw
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .flat_map(|s| resolve_var_ref(s.trim(), &unified_vars))
                    .collect();

                // Look up resolved names in hosts table
                for name in &resolved_names {
                    if let Ok(Some(row)) = sqlx::query(
                        "SELECT id FROM hosts WHERE name = ? AND (team_id IS NULL OR team_id = ?)",
                    )
                    .bind(name)
                    .bind(team_id.as_deref())
                    .fetch_optional(&state.pool)
                    .await
                    {
                        let id: String = row.get(0);
                        if !host_ids.contains(&id) {
                            host_ids.push(id);
                        }
                    } else {
                        tracing::warn!(
                            "SSH step '{}': resolved dynamic host name '{}' not found",
                            t.get::<String, _>(2),
                            name
                        );
                    }
                }

                // Look up resolved labels
                for label in &resolved_labels {
                    let rows = sqlx::query(
                        "SELECT hosts.id FROM hosts, json_each(hosts.labels) \
                         WHERE json_each.value = ? AND (hosts.team_id IS NULL OR hosts.team_id = ?)",
                    )
                    .bind(label)
                    .bind(team_id.as_deref())
                    .fetch_all(&state.pool)
                    .await
                    .unwrap_or_default();
                    for r in &rows {
                        let id: String = r.get(0);
                        if !host_ids.contains(&id) {
                            host_ids.push(id);
                        }
                    }
                }
            }

            if host_ids.len() == 1 {
                // Single host — return as object (backward-compatible)
                if let Ok(Some(host_row)) = sqlx::query(
                    "SELECT host, port, username, credential_type, credential_data, credential_passphrase
                     FROM hosts WHERE id = ? AND (team_id IS NULL OR team_id = ?)",
                )
                .bind(&host_ids[0])
                .bind(team_id.as_deref())
                .fetch_optional(&state.pool)
                .await
                {
                    task_json["host"] = build_host_json(&host_row, &state.config.auth.jwt_secret);
                } else {
                    task_json["host_resolution_error"] = serde_json::Value::String(
                        "one or more hosts are not available to this pipeline team".into(),
                    );
                }
            } else if host_ids.len() > 1 {
                // Multiple hosts — return as array
                let mut host_list: Vec<serde_json::Value> = Vec::new();
                for hid in &host_ids {
                    if let Ok(Some(host_row)) = sqlx::query(
                        "SELECT host, port, username, credential_type, credential_data \
                         FROM hosts WHERE id = ? AND (team_id IS NULL OR team_id = ?)",
                    )
                    .bind(hid)
                    .bind(team_id.as_deref())
                    .fetch_optional(&state.pool)
                    .await
                    {
                        host_list.push(build_host_json(&host_row, &state.config.auth.jwt_secret));
                    }
                }
                if host_list.len() != host_ids.len() {
                    task_json["host_resolution_error"] = serde_json::Value::String(
                        "one or more hosts are not available to this pipeline team".into(),
                    );
                }
                task_json["host"] = serde_json::Value::Array(host_list);
            } else {
                // No hosts resolved from the database
                let task_name = task_json["name"].as_str().unwrap_or("unnamed");
                tracing::warn!(
                    "SSH step '{task_name}': no hosts resolved (static host_id='{}', raw names='{}', raw labels='{}')",
                    host_id_str, host_names_raw, host_labels_raw
                );
                task_json["host_resolution_error"] =
                    serde_json::Value::String("no hosts resolved for this pipeline team".into());
                task_json["host"] = serde_json::Value::Array(vec![]);
            }
        }

        // Attach step allowlist values for mask (Runner does not auto-inject `$NAME`).
        task_json["step_secrets"] = step_secrets_map.clone();

        task_json["log_mask_values"] = serde_json::Value::Array(
            log_mask_values
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        );

        tasks.push(task_json);
    }

    tracing::info!("Job dispatched: {job_id} → runner {}", auth.runner_id);

    log_runner_event(
        &state.pool,
        &auth.runner_id,
        "job_dispatched",
        &format!(
            "Job {job_id} claimed, pipeline={pipeline_id}, {} steps",
            tasks.len()
        ),
    )
    .await;

    // Also place masking inputs into `secrets` so Runner log redaction (by secret key)
    // always covers them — same path as Infrastructure Secrets.
    if let Some(obj) = secrets.as_object_mut() {
        for key in &masking_input_keys {
            if let Some(val) = trigger_params_plain.get(key) {
                obj.entry(key.clone())
                    .or_insert_with(|| serde_json::Value::String(val.clone()));
            }
        }
    }

    // Job metadata for the runner's BUZ_* env (ADR §5.2) — plugin steps
    // render messages like "build #<job> passed" from this.
    let team_name: Option<String> = sqlx::query_scalar("SELECT name FROM teams WHERE id = ?")
        .bind(&team_id)
        .fetch_optional(&state.pool)
        .await?
        .flatten();

    Ok(serde_json::json!({
        "job_id": job_id,
        "cancelled_jobs": cancelled_jobs,
        "pipeline_id": pipeline_id,
        "metadata": {
            "pipeline_name": pipeline_name,
            "team": team_name.or(team_id),
            "trigger": pipeline_trigger_type,
            "triggered_by": trigger_by,
            "run_number": run_number,
            "run_attempt": run_attempt,
        },
        "tasks": tasks,
        "inputs": trigger_params,
        "masking_input_keys": masking_input_keys,
        "resource_profile": job_resource_request(&snapshot_raw).0,
        "env": env,
        "secret_env_keys": secret_env_keys,
        "secrets": secrets,
        "debug_unredact": debug_unredact,
        "verbose": verbose_logs,
    }))
}

/// Load same-team secrets by name into `into_by_name` (and optionally a parallel map).
/// Skips names already present. Used for GHA-style implicit allowlist via `${secrets.X}`
/// / purpose slots when top-level `secrets:` is omitted.
/// the step allowlist but not resolvable in the pipeline's team. Returns
/// `None` when `missing` is empty. The message never leaks secret values.
async fn merge_team_secrets_by_name(
    pool: &sqlx::SqlitePool,
    pipeline_id: &str,
    jwt_secret: &str,
    names: &[String],
    into_by_name: &mut serde_json::Value,
    mut into_parallel: Option<&mut serde_json::Value>,
    env_secret_overrides: &std::collections::HashMap<String, String>,
) -> Result<(), AppError> {
    let needed: Vec<&str> = names
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter(|s| into_by_name.get(*s).and_then(|v| v.as_str()).is_none())
        .collect();
    if needed.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; needed.len()].join(",");
    let query = format!(
        "SELECT s.name, s.value FROM secrets s
         JOIN pipelines p ON p.team_id = s.team_id AND p.team_id != ''
         WHERE p.id = ? AND s.name IN ({placeholders})"
    );
    let mut q = sqlx::query_as::<_, (String, String)>(&query).bind(pipeline_id);
    for name in &needed {
        q = q.bind(*name);
    }
    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load secrets by name: {e}")))?;
    for (name, stored) in rows {
        match crypto::decrypt(&stored, jwt_secret) {
            Ok(decrypted) => {
                let value = env_secret_overrides
                    .get(&name)
                    .cloned()
                    .unwrap_or(decrypted);
                if let Some(obj) = into_by_name.as_object_mut() {
                    obj.entry(name.clone())
                        .or_insert(serde_json::Value::String(value.clone()));
                }
                if let Some(parallel) = into_parallel.as_deref_mut() {
                    if let Some(obj) = parallel.as_object_mut() {
                        obj.entry(name).or_insert(serde_json::Value::String(value));
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to decrypt secret '{name}': {e}");
            }
        }
    }
    Ok(())
}

/// Build a [`ValueMap`] for expanding `env:` / step `env:` expressions at claim.
fn claim_value_map(
    secrets_by_name: &serde_json::Value,
    env: &serde_json::Value,
    trigger_params: &serde_json::Value,
) -> shared::value_source::ValueMap {
    let mut map = shared::value_source::ValueMap::new();
    if let Some(obj) = secrets_by_name.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                map.secrets.insert(k.clone(), s.to_string());
            }
        }
    }
    if let Some(obj) = env.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                map.vars.insert(k.clone(), s.to_string());
            }
        }
    }
    if let Some(obj) = trigger_params.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                map.vars.insert(k.clone(), s.to_string());
                map.vars
                    .insert(shared::protocol::input_env_key(k), s.to_string());
            }
        }
    }
    map
}

/// Resolve a variable reference like `$VAR_NAME` or `${VAR_NAME}` against
/// the trigger inputs. Returns the resolved value(s) as a Vec — for the
/// dynamic host resolution, a single input value could map to a host name
/// or a comma-separated list.
fn resolve_var_ref(raw: &str, inputs: &serde_json::Value) -> Vec<String> {
    // Strip wrapping ${...} or leading $
    let var_name = raw
        .strip_prefix("${")
        .and_then(|s| s.strip_suffix('}'))
        .or_else(|| raw.strip_prefix('$'))
        .unwrap_or(raw);

    // Look up the variable in trigger inputs
    if let Some(obj) = inputs.as_object() {
        if let Some(val) = obj.get(var_name).and_then(|v| v.as_str()) {
            if val.is_empty() || val == raw {
                return vec![];
            }
            // Support comma-separated multi-host values
            return val
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }

    // If the raw value doesn't contain '$', return it as a literal
    // (e.g. "dev-vm" stays "dev-vm")
    if !raw.contains('$') {
        return vec![raw.to_string()];
    }

    vec![]
}

/// Build host connection JSON for dispatch to the runner.
///
/// Credential data (private key or password) is decrypted from the encrypted
/// database value and sent as `credential_value` so the runner can use it
/// directly without reading local files.
fn build_host_json(host_row: &sqlx::sqlite::SqliteRow, jwt_secret: &str) -> serde_json::Value {
    let host: String = host_row.get(0);
    let port: i64 = host_row.get(1);
    let username: String = host_row.get(2);
    let cred_type: String = host_row.get(3);
    let cred_data_encrypted: String = host_row.get(4);
    let passphrase_encrypted: String = host_row.get(5);

    let cred_data = if cred_data_encrypted.is_empty() {
        tracing::warn!(
            "Host '{}': credential_data is empty — no credential will be sent to runner",
            host
        );
        String::new()
    } else {
        match crypto::decrypt(&cred_data_encrypted, jwt_secret) {
            Ok(decrypted) => {
                tracing::debug!(
                    "Host '{}': credential_data decrypted successfully ({} bytes)",
                    host,
                    decrypted.len()
                );
                decrypted
            }
            Err(e) => {
                tracing::error!("Host '{}': failed to decrypt credential_data: {e}", host);
                String::new()
            }
        }
    };

    let passphrase = if passphrase_encrypted.is_empty() {
        String::new()
    } else {
        match crypto::decrypt(&passphrase_encrypted, jwt_secret) {
            Ok(decrypted) => decrypted,
            Err(e) => {
                tracing::error!("Host '{}': failed to decrypt passphrase: {e}", host);
                String::new()
            }
        }
    };

    let mut host_json = serde_json::json!({
        "host": host,
        "port": port as u16,
        "username": username,
        "credential_type": cred_type,
    });

    if !cred_data.is_empty() {
        host_json["credential_value"] = serde_json::Value::String(cred_data);
    }
    if !passphrase.is_empty() {
        host_json["credential_passphrase"] = serde_json::Value::String(passphrase);
    }

    host_json
}

pub async fn list_runners(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = sqlx::query(
        "SELECT r.id, r.name, r.labels, r.status, r.version, r.system_info, r.last_seen, r.is_frozen,
                r.max_concurrent_jobs, r.clean_workspace_after_job,
                r.shell_enabled, r.ssh_enabled, r.docker_enabled, r.wasm_enabled, r.ssh_host_key_policy,
                r.created_at, r.updated_at, r.team_id,
                (SELECT COUNT(*) FROM jobs j WHERE j.runner_id = r.id AND j.status = 'running') AS running_jobs,
                r.execution_mode, r.host_id, r.runner_instance_id, r.claim_strategy,
                r.netns_isolation,
                r.capability_reprobe_requested, r.capability_reprobe_requested_at,
                r.capability_reprobe_served_at
         FROM runners r ORDER BY r.created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;

    let runners: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| -> Result<serde_json::Value, AppError> {
            let labels: String = r.get(2);
            let system_info_raw: String = r.get(5);
            let system_info = serde_json::from_str::<serde_json::Value>(&system_info_raw).ok();
            let policy = RunnerPolicyConfig {
                max_concurrent_jobs: r.get::<i64, _>(8).max(1) as u32,
                clean_workspace_after_job: i64_bool(r.get(9)),
                shell_enabled: i64_bool(r.get(10)),
                ssh_enabled: i64_bool(r.get(11)),
                docker_enabled: i64_bool(r.get(12)),
                wasm_enabled: i64_bool(r.get(13)),
                ssh_host_key_policy: {
                    let p: String = r.get(14);
                    if p == "insecure" {
                        p
                    } else {
                        "known_hosts".into()
                    }
                },
                execution_mode: parse_execution_mode(r.get::<String, _>(19))?,
                claim_strategy: normalize_claim_strategy(Some(r.get::<String, _>(22)))
                    .unwrap_or_else(|_| "backfill".into()),
                netns_isolation: i64_bool(r.get(24)),
            };
            Ok(serde_json::json!({
                "id": r.get::<String, _>(0),
                "name": r.get::<String, _>(1),
                "labels": serde_json::from_str::<Vec<String>>(&labels).unwrap_or_default(),
                "status": r.get::<String, _>(3),
                "version": r.get::<Option<String>, _>(4),
                "system_info": system_info,
                "last_seen": r.get::<Option<String>, _>(6),
                "is_frozen": r.get::<i64, _>(7) != 0,
                "capability_reprobe_requested": r.get::<i64, _>("capability_reprobe_requested") != 0,
                "capability_reprobe_requested_at": r.get::<Option<String>, _>("capability_reprobe_requested_at"),
                "capability_reprobe_served_at": r.get::<Option<String>, _>("capability_reprobe_served_at"),
                "max_concurrent_jobs": policy.max_concurrent_jobs,
                "clean_workspace_after_job": policy.clean_workspace_after_job,
                "shell_enabled": policy.shell_enabled,
                "ssh_enabled": policy.ssh_enabled,
                "docker_enabled": policy.docker_enabled,
                "wasm_enabled": policy.wasm_enabled,
                "netns_isolation": policy.netns_isolation,
                "ssh_host_key_policy": policy.ssh_host_key_policy,
                "claim_strategy": policy.claim_strategy,
                "execution_mode": policy.execution_mode.as_str(),
                "team_id": r.get::<Option<String>, _>(17),
                "created_at": r.get::<Option<String>, _>(15),
                "updated_at": r.get::<Option<String>, _>(16),
                "running_jobs": r.get::<i64, _>(18),
                "host_id": r.get::<String, _>(20),
                "runner_instance_id": r.get::<String, _>(21),
            }))
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    Ok(Json(ApiResponse::success(runners)))
}

pub async fn get_runner(
    _auth: PlatformAdmin,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let r = sqlx::query(
        "SELECT id, name, labels, status, version, system_info, last_seen,
                created_at, updated_at, team_id, host_id, runner_instance_id,
                capability_reprobe_requested, capability_reprobe_requested_at,
                capability_reprobe_served_at
         FROM runners WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("runner not found".into()))?;

    let labels: String = r.get(2);
    let system_info_raw: String = r.get(5);
    let system_info = serde_json::from_str::<serde_json::Value>(&system_info_raw).ok();
    let running_jobs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE runner_id = ? AND status = 'running'")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);
    let policy = load_runner_policy(&state.pool, &id).await?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.get::<String, _>(0),
        "name": r.get::<String, _>(1),
        "labels": serde_json::from_str::<Vec<String>>(&labels).unwrap_or_default(),
        "status": r.get::<String, _>(3),
        "version": r.get::<Option<String>, _>(4),
        "system_info": system_info,
        "last_seen": r.get::<Option<String>, _>(6),
        "created_at": r.get::<Option<String>, _>(7),
        "updated_at": r.get::<Option<String>, _>(8),
        "team_id": r.get::<Option<String>, _>(9),
        "host_id": r.get::<String, _>(10),
        "runner_instance_id": r.get::<String, _>(11),
        "capability_reprobe_requested": r.get::<i64, _>("capability_reprobe_requested") != 0,
        "capability_reprobe_requested_at": r.get::<Option<String>, _>("capability_reprobe_requested_at"),
        "capability_reprobe_served_at": r.get::<Option<String>, _>("capability_reprobe_served_at"),
        "running_jobs": running_jobs,
        "max_concurrent_jobs": policy.max_concurrent_jobs,
        "clean_workspace_after_job": policy.clean_workspace_after_job,
        "shell_enabled": policy.shell_enabled,
        "ssh_enabled": policy.ssh_enabled,
        "docker_enabled": policy.docker_enabled,
        "wasm_enabled": policy.wasm_enabled,
        "ssh_host_key_policy": policy.ssh_host_key_policy,
        "execution_mode": policy.execution_mode.as_str(),
        "claim_strategy": policy.claim_strategy,
        "netns_isolation": policy.netns_isolation,
    }))))
}

/// Validates switching `runner_id` to `mode` against the runner's live state:
/// sandbox capability freshness for non-Host modes and no running jobs. Must
/// be called inside the caller's `BEGIN IMMEDIATE` transaction that also
/// performs the update; on error the caller must roll the transaction back.
async fn validate_execution_mode_transition(
    tx: &mut sqlx::SqliteConnection,
    runner_id: &str,
    mode: RunnerExecutionMode,
) -> Result<(), AppError> {
    let (status, last_seen, system_info_raw): (String, Option<String>, String) =
        sqlx::query_as("SELECT status, last_seen, system_info FROM runners WHERE id = ?")
            .bind(runner_id)
            .fetch_one(&mut *tx)
            .await?;
    if mode != RunnerExecutionMode::Host {
        if status != "online" {
            return Err(AppError::Conflict(
                "runner must be online and report fresh sandbox capabilities before selecting this execution mode".into(),
            ));
        }
        let fresh: bool =
            sqlx::query_scalar("SELECT COALESCE(datetime(?) >= datetime('now', '-90 seconds'), 0)")
                .bind(last_seen.as_deref())
                .fetch_one(&mut *tx)
                .await?;
        let capabilities = serde_json::from_str::<SystemInfo>(&system_info_raw)
            .ok()
            .and_then(|info| info.execution_capabilities);
        let Some(capabilities) = capabilities else {
            return Err(AppError::Conflict(
                "runner has not reported sandbox capabilities yet; wait for its next heartbeat"
                    .into(),
            ));
        };
        // One per-tier lookup for every sandboxed tier — Host never reaches here.
        let Some((backend, reason)) = capabilities.backend(mode) else {
            return Err(AppError::Conflict(format!(
                "runner does not support {} execution: host has no sandbox backend",
                mode.as_str()
            )));
        };
        let checked_fresh: bool = sqlx::query_scalar(
            "SELECT COALESCE(datetime(?) >= datetime('now', '-5 minutes') AND datetime(?) <= datetime('now', '+30 seconds'), 0)",
        )
        .bind(capabilities.checked_at.as_deref())
        .bind(capabilities.checked_at.as_deref())
        .fetch_one(&mut *tx)
        .await?;
        if !fresh || !checked_fresh || backend != ExecutionBackendStatus::Supported {
            let detail = reason.unwrap_or("capability check is stale or unsupported");
            return Err(AppError::Conflict(format!(
                "runner does not support {} execution: {detail}",
                mode.as_str()
            )));
        }
    }
    let running: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE runner_id = ? AND status = 'running'")
            .bind(runner_id)
            .fetch_one(&mut *tx)
            .await?;
    if running > 0 {
        return Err(AppError::Conflict(format!(
            "execution mode cannot change while {running} job(s) are running"
        )));
    }
    Ok(())
}

pub async fn update_runner(
    auth: PlatformAdmin,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateRunnerRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM runners WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?;
    if exists.is_none() {
        return Err(AppError::NotFound("runner not found".into()));
    }
    // Snapshot before the field-by-field updates so the audit record states
    // the diff, not the whole row.
    let before_policy = load_runner_policy(&state.pool, &id).await?;
    let before_team: Option<String> =
        sqlx::query_scalar("SELECT team_id FROM runners WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await?;
    let before_labels: String = sqlx::query_scalar("SELECT labels FROM runners WHERE id = ?")
        .bind(&id)
        .fetch_one(&state.pool)
        .await?;
    let before = serde_json::json!({
        "team_id": before_team,
        "labels": serde_json::from_str::<Vec<String>>(&before_labels).unwrap_or_default(),
        "max_concurrent_jobs": before_policy.max_concurrent_jobs,
        "clean_workspace_after_job": before_policy.clean_workspace_after_job,
        "execution_mode": before_policy.execution_mode.as_str(),
        "shell_enabled": before_policy.shell_enabled,
        "ssh_enabled": before_policy.ssh_enabled,
        "docker_enabled": before_policy.docker_enabled,
        "wasm_enabled": before_policy.wasm_enabled,
        "ssh_host_key_policy": before_policy.ssh_host_key_policy,
        "netns_isolation": before_policy.netns_isolation,
    });

    if let Some(requested_team) = &req.team_id {
        let validated = validate_runner_team(&state.pool, requested_team.as_deref()).await?;
        let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
        let current_team_id: Option<String> =
            sqlx::query_scalar("SELECT team_id FROM runners WHERE id = ?")
                .bind(&id)
                .fetch_one(&mut *tx)
                .await?;
        if validated != current_team_id {
            let running_jobs: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM jobs WHERE runner_id = ? AND status = 'running'",
            )
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;
            if running_jobs > 0 {
                tx.rollback().await?;
                return Err(AppError::Conflict(format!(
                    "runner team binding cannot change while {running_jobs} job(s) are running"
                )));
            }
            sqlx::query(
                "UPDATE runners SET team_id = ?, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(validated)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }

    if let Some(labels) = &req.labels {
        let labels_json = serde_json::to_string(labels).unwrap_or_else(|_| "[]".into());
        sqlx::query("UPDATE runners SET labels = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(&labels_json)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    if req.max_concurrent_jobs.is_some() {
        let max_concurrent = normalize_max_concurrent(req.max_concurrent_jobs)?;
        sqlx::query(
            "UPDATE runners SET max_concurrent_jobs = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(max_concurrent as i64)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }

    if let Some(mode) = req.execution_mode {
        let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
        let current: String = sqlx::query_scalar("SELECT execution_mode FROM runners WHERE id = ?")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;
        if current != mode.as_str() {
            if let Err(error) = validate_execution_mode_transition(&mut tx, &id, mode).await {
                tx.rollback().await?;
                return Err(error);
            }
        }
        sqlx::query(
            "UPDATE runners SET execution_mode = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(mode.as_str())
        .bind(&id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }

    if let Some(v) = req.clean_workspace_after_job {
        sqlx::query(
            "UPDATE runners SET clean_workspace_after_job = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(if v { 1i64 } else { 0 })
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }
    if let Some(v) = req.shell_enabled {
        sqlx::query(
            "UPDATE runners SET shell_enabled = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(if v { 1i64 } else { 0 })
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }
    if let Some(v) = req.ssh_enabled {
        sqlx::query(
            "UPDATE runners SET ssh_enabled = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(if v { 1i64 } else { 0 })
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }
    if let Some(v) = req.docker_enabled {
        sqlx::query(
            "UPDATE runners SET docker_enabled = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(if v { 1i64 } else { 0 })
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }
    if let Some(v) = req.wasm_enabled {
        sqlx::query(
            "UPDATE runners SET wasm_enabled = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(if v { 1i64 } else { 0 })
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }
    if let Some(v) = req.netns_isolation {
        sqlx::query(
            "UPDATE runners SET netns_isolation = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(if v { 1i64 } else { 0 })
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }
    if req.ssh_host_key_policy.is_some() {
        let p = normalize_ssh_host_key_policy(req.ssh_host_key_policy.clone())?;
        sqlx::query(
            "UPDATE runners SET ssh_host_key_policy = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&p)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }
    if req.claim_strategy.is_some() {
        let strategy = normalize_claim_strategy(req.claim_strategy.clone())?;
        sqlx::query(
            "UPDATE runners SET claim_strategy = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&strategy)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    }

    log_runner_event(&state.pool, &id, "updated", "Runner policy updated from UI").await;

    let r = sqlx::query(
        "SELECT id, name, labels, status, version, system_info, last_seen,
                created_at, updated_at, team_id FROM runners WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await?;

    let labels: String = r.get(2);
    let system_info_raw: String = r.get(5);
    let system_info = serde_json::from_str::<serde_json::Value>(&system_info_raw).ok();
    let running_jobs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE runner_id = ? AND status = 'running'")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);
    let policy = load_runner_policy(&state.pool, &id).await?;

    // Diff against the snapshot taken before the updates.
    let after = serde_json::json!({
        "team_id": r.get::<Option<String>, _>(9),
        "labels": serde_json::from_str::<Vec<String>>(&labels).unwrap_or_default(),
        "max_concurrent_jobs": policy.max_concurrent_jobs,
        "clean_workspace_after_job": policy.clean_workspace_after_job,
        "execution_mode": policy.execution_mode.as_str(),
        "shell_enabled": policy.shell_enabled,
        "ssh_enabled": policy.ssh_enabled,
        "docker_enabled": policy.docker_enabled,
        "wasm_enabled": policy.wasm_enabled,
        "ssh_host_key_policy": policy.ssh_host_key_policy,
    });
    audit::record(
        &state.pool,
        AuditEvent::new(Action::RunnerUpdate, &auth)
            .on(Resource::runner(&id, &r.get::<String, _>(1)))
            .team(r.get::<Option<String>, _>(9))
            .changes(Some(before), Some(after)),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.get::<String, _>(0),
        "name": r.get::<String, _>(1),
        "labels": serde_json::from_str::<Vec<String>>(&labels).unwrap_or_default(),
        "status": r.get::<String, _>(3),
        "version": r.get::<Option<String>, _>(4),
        "system_info": system_info,
        "last_seen": r.get::<Option<String>, _>(6),
        "created_at": r.get::<Option<String>, _>(7),
        "updated_at": r.get::<Option<String>, _>(8),
        "team_id": r.get::<Option<String>, _>(9),
        "running_jobs": running_jobs,
        "max_concurrent_jobs": policy.max_concurrent_jobs,
        "clean_workspace_after_job": policy.clean_workspace_after_job,
        "shell_enabled": policy.shell_enabled,
        "ssh_enabled": policy.ssh_enabled,
        "docker_enabled": policy.docker_enabled,
        "wasm_enabled": policy.wasm_enabled,
        "ssh_host_key_policy": policy.ssh_host_key_policy,
        "execution_mode": policy.execution_mode.as_str(),
        "claim_strategy": policy.claim_strategy,
    }))))
}

pub async fn delete_runner(
    auth: PlatformAdmin,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    // Snapshot first: the audit record outlives the row and is the only
    // remaining description of what was deleted.
    let removed = sqlx::query("SELECT name, team_id, status FROM runners WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?;
    // Nullify runner references in past jobs so FK constraint is not violated.
    // Jobs table FK: runner_id TEXT REFERENCES runners(id) — no ON DELETE CASCADE/SET NULL.
    let _ = sqlx::query("UPDATE jobs SET runner_id = NULL WHERE runner_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await;

    let result = sqlx::query("DELETE FROM runners WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("runner not found".into()));
    }

    if let Some(row) = &removed {
        let name: String = row.get(0);
        audit::record(
            &state.pool,
            AuditEvent::new(Action::RunnerDelete, &auth)
                .on(Resource::runner(&id, &name))
                .before(Some(serde_json::json!({
                    "name": name,
                    "team_id": row.get::<Option<String>, _>(1),
                    "status": row.get::<String, _>(2),
                }))),
        )
        .await;
    }
    tracing::info!("Runner deleted: {id}");
    Ok(Json(ApiResponse::success(())))
}

pub async fn toggle_freeze(
    auth: PlatformAdmin,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = sqlx::query("SELECT name, is_frozen FROM runners WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("runner not found".into()))?;

    let current_frozen: i64 = row.get(1);
    let new_frozen = if current_frozen != 0 { 0 } else { 1 };

    sqlx::query("UPDATE runners SET is_frozen = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(new_frozen)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    crate::services::poll_events::wake_all();

    let action = if new_frozen != 0 {
        "frozen"
    } else {
        "unfrozen"
    };
    let name: String = row.get(0);
    tracing::info!("Runner {name} ({id}) {action}");

    log_runner_event(&state.pool, &id, action, &format!("Runner {action}")).await;
    audit::record(
        &state.pool,
        AuditEvent::new(Action::RunnerFreeze, &auth)
            .on(Resource::runner(&id, &name))
            .changes(
                Some(serde_json::json!({ "is_frozen": current_frozen != 0 })),
                Some(serde_json::json!({ "is_frozen": new_frozen != 0 })),
            ),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "is_frozen": new_frozen != 0
    }))))
}

/// Request a clean runner restart. The flag is served on the runner's next
/// heartbeat (≤30s); the runner then exits with code 0 and its supervisor
/// brings it back up. Refused with 409 while the runner has running jobs.
pub async fn restart(
    auth: PlatformAdmin,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = sqlx::query("SELECT name FROM runners WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("runner not found".into()))?;
    let name: String = row.get(0);

    let running_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM jobs WHERE runner_id = ? AND status = 'running' ORDER BY created_at",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await?;

    if !running_ids.is_empty() {
        return Err(AppError::Conflict(format!(
            "runner has {} running job(s): {} — cancel them before restarting",
            running_ids.len(),
            running_ids.join(", ")
        )));
    }

    sqlx::query(
        "UPDATE runners SET restart_requested = 1, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(&id)
    .execute(&state.pool)
    .await?;

    crate::services::poll_events::wake_all();
    tracing::info!("Runner {name} ({id}) restart requested");
    log_runner_event(
        &state.pool,
        &id,
        "restart_requested",
        "Restart requested — runner will exit on next heartbeat",
    )
    .await;
    audit::record(
        &state.pool,
        AuditEvent::new(Action::RunnerRestart, &auth).on(Resource::runner(&id, &name)),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "restart_requested": true
    }))))
}

/// Request an on-demand sandbox capability re-probe: sets a one-shot flag the
/// Runner's next heartbeat (≤30s) serves; the Runner then probes its backends
/// immediately and reports fresh results on the following heartbeat. Unlike
/// restart this does not block dispatch — probing is observational and safe
/// while jobs run.
pub async fn reprobe_capabilities(
    auth: PlatformAdmin,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = sqlx::query("SELECT name FROM runners WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("runner not found".into()))?;
    let name: String = row.get(0);

    sqlx::query(
        "UPDATE runners SET capability_reprobe_requested = 1,
            capability_reprobe_requested_at = datetime('now'),
            capability_reprobe_served_at = NULL,
            updated_at = datetime('now') WHERE id = ?",
    )
    .bind(&id)
    .execute(&state.pool)
    .await?;

    tracing::info!("Runner {name} ({id}) capability re-probe requested");
    log_runner_event(
        &state.pool,
        &id,
        "capability_reprobe_requested",
        "Capability re-probe requested — fresh results on next heartbeat",
    )
    .await;
    audit::record(
        &state.pool,
        AuditEvent::new(Action::RunnerReprobeCapabilities, &auth).on(Resource::runner(&id, &name)),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "reprobe_requested": true
    }))))
}

/// Re-issue the one-shot register_token for an existing runner — the admin
/// recovery path for re-registration. Use when a runner lost its cached JWT
/// (pre-1.6 restart, wiped workspace) or after a jwt_secret rotation: update
/// the runner's configured register_token, restart it once, and it re-enters
/// the normal Bearer refresh loop. The new token is returned exactly once and
/// never logged.
#[derive(serde::Serialize)]
pub struct ReissueRegisterTokenResponse {
    pub register_token: String,
}

pub async fn reissue_register_token(
    auth: PlatformAdmin,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<ReissueRegisterTokenResponse>>, AppError> {
    let register_token = Uuid::new_v4().to_string();
    let name: Option<String> = sqlx::query_scalar(
        "UPDATE runners SET register_token = ?, updated_at = datetime('now') WHERE id = ? RETURNING name",
    )
    // Hash-only at rest — the plaintext is returned exactly once below.
    .bind(shared::api_token::hash_token(&register_token))
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;
    let name = name.ok_or_else(|| AppError::NotFound("runner not found".into()))?;

    tracing::info!("Runner {name} ({id}) register token re-issued");
    log_runner_event(
        &state.pool,
        &id,
        "register_token_reissued",
        "Register token re-issued (value shown once in the API response, never logged)",
    )
    .await;
    crate::audit::record(
        &state.pool,
        crate::audit::AuditEvent::from_auth(&auth.0, Action::RunnerRegisterTokenReissued)
            .resource("runner", &id, &name),
    )
    .await;

    Ok(Json(ApiResponse::success(ReissueRegisterTokenResponse {
        register_token,
    })))
}

pub async fn get_events(
    _auth: PlatformAdmin,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    // Verify runner exists
    let _ = sqlx::query("SELECT id FROM runners WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("runner not found".into()))?;

    let page: i64 = params.get("page").and_then(|p| p.parse().ok()).unwrap_or(1);
    let page_size: i64 = params
        .get("page_size")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    let offset = (page - 1) * page_size;

    // Get total count
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runner_events WHERE runner_id = ?")
        .bind(&id)
        .fetch_one(&state.pool)
        .await?;

    let rows = sqlx::query(
        "SELECT id, event_type, message, created_at
         FROM runner_events
         WHERE runner_id = ?
         ORDER BY created_at DESC
         LIMIT ? OFFSET ?",
    )
    .bind(&id)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;

    let events: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "event_type": r.get::<String, _>(1),
                "message": r.get::<String, _>(2),
                "created_at": r.get::<Option<String>, _>(3),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(serde_json::json!({
        "items": events,
        "total": total,
        "page": page,
        "page_size": page_size,
        "total_pages": ((total as f64) / (page_size as f64)).ceil() as i64,
    }))))
}

/// Poll response envelope for no-job paths; cancel notices ride the claim channel.
fn cancel_only_response(cancelled_jobs: Vec<String>) -> serde_json::Value {
    serde_json::json!({ "cancelled_jobs": cancelled_jobs })
}

/// Clear only the notices this runner explicitly acked after applying them.
async fn acknowledge_cancelled(
    pool: &sqlx::SqlitePool,
    runner_id: &str,
    ids: &[String],
) -> Result<(), AppError> {
    for id in ids {
        sqlx::query(
            "UPDATE jobs SET cancel_ack_pending = 0 \
             WHERE id = ? AND runner_id = ? AND status = 'cancelled' AND cancel_ack_pending = 1",
        )
        .bind(id)
        .bind(runner_id)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Read undelivered cancellation IDs. They remain pending until a subsequent
/// Runner poll acknowledges receipt, so a lost response is safely retried.
async fn pending_cancelled_for_delivery(
    pool: &sqlx::SqlitePool,
    runner_id: &str,
) -> Result<Vec<String>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM jobs \
         WHERE runner_id = ? AND status = 'cancelled' AND cancel_ack_pending = 1 \
         ORDER BY updated_at, id LIMIT 32",
    )
    .bind(runner_id)
    .fetch_all(pool)
    .await?)
}
