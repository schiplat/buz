use axum::extract::ConnectInfo;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::{extract::FromRequestParts, extract::State, response::IntoResponse, Json};
use shared::auth::TokenManager;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{
    CreateRunnerRequest, HeartbeatRequest, HeartbeatResponse, RegisterRequest, RegisterResponse,
    RunnerExecutionMode, RunnerPolicyConfig, UpdateRunnerRequest,
};
use std::convert::Infallible;
use std::net::SocketAddr;

use crate::audit::{self, Action, AuditEvent, Resource};
use crate::config::Config;
use crate::crypto;
use crate::middleware::{PlatformAdmin, RunnerClaims};
use crate::state::AppState;
use crate::store::exec::DbPool;
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

/// Best-effort note of why a poller skipped a pending job: the store owns the
/// write and the log-on-change behaviour.
async fn record_dispatch_reason(pool: &sqlx::SqlitePool, job_id: &str, reason: &str) {
    crate::store::runners::record_dispatch_reason(&DbPool::from_sqlite(pool), job_id, reason).await;
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
    crate::store::runners::log_runner_event(
        &DbPool::from_sqlite(pool),
        runner_id,
        event_type,
        message,
    )
    .await
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
    if !crate::store::runners::team_exists(&DbPool::from_sqlite(pool), team_id).await? {
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
    let row = crate::store::runners::policy_row(&DbPool::from_sqlite(pool), runner_id)
        .await?
        .ok_or_else(|| AppError::NotFound("runner not found".into()))?;

    let (
        max_concurrent,
        clean_workspace,
        shell,
        ssh,
        docker,
        wasm,
        ssh_policy,
        mode,
        strategy,
        netns,
    ) = row;
    Ok(RunnerPolicyConfig {
        max_concurrent_jobs: max_concurrent.max(1) as u32,
        clean_workspace_after_job: i64_bool(clean_workspace),
        shell_enabled: i64_bool(shell),
        ssh_enabled: i64_bool(ssh),
        docker_enabled: i64_bool(docker),
        wasm_enabled: i64_bool(wasm),
        ssh_host_key_policy: {
            if ssh_policy == "insecure" {
                ssh_policy
            } else {
                "known_hosts".into()
            }
        },
        execution_mode: parse_execution_mode(mode)?,
        claim_strategy: normalize_claim_strategy(Some(strategy))
            .unwrap_or_else(|_| "backfill".into()),
        netns_isolation: i64_bool(netns),
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
    let team_id = crate::store::runners::team_of(&state.db(), &created.id).await?;

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

    // Hash-only at rest: the plaintext is returned exactly once in the API
    // response and never stored (same discipline as buz_/buzsd_ tokens).
    crate::store::runners::insert(
        &DbPool::from_sqlite(pool),
        &crate::store::runners::NewRunner {
            id: &id,
            name: &req.name,
            register_token_hash: &shared::api_token::hash_token(&register_token),
            labels_json: &labels_json,
            version: req.version.as_deref(),
            team_id: team_id.as_deref(),
            execution_mode,
            max_concurrent_jobs: max_concurrent as i64,
            clean_workspace: clean,
            shell,
            ssh,
            docker,
            wasm,
            ssh_host_key_policy: &ssh_policy,
            claim_strategy: &claim_strategy,
        },
    )
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
        let exists = crate::store::runners::count_by_name(&DbPool::from_sqlite(pool), name).await?;
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
    let existing = crate::store::runners::register_row_by_name(&state.db(), name).await?;

    let runner_id: String;

    if let Some(ref row) = existing {
        let existing_token: &Option<String> = &row.2;
        let row_id: &String = &row.0;

        if existing_token.as_ref().is_none_or(|t| t.is_empty()) {
            // First-time registration — must match the one-shot register_token.
            let stored_rt: &String = &row.1;
            if !register_token_matches(stored_rt, register_token) {
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
                bearer_proves_runner(&headers, &state.config.auth.jwt_secret, row_id) || {
                    register_token_matches(&row.1, register_token)
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

        runner_id = row_id.clone();
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
    crate::store::runners::mark_registered(
        &state.db(),
        &runner_id,
        &shared::api_token::hash_token(&runner_jwt),
        version,
        &host_id,
        &runner_instance_id,
    )
    .await?;

    tracing::info!(runner = %name, host_id = %host_id, instance = %runner_instance_id, "Runner registered: {} ({runner_id})", name);

    let policy = load_runner_policy(&state.pool, &runner_id).await?;
    let labels_db = crate::store::runners::labels_of(&state.db(), &runner_id).await?;

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
    crate::store::runners::mark_heartbeat(
        &state.db(),
        &auth.runner_id,
        &req.status,
        &system_info_json,
        version,
        &host_id,
        &runner_instance_id,
        &sandbox_instance_id,
    )
    .await?;

    let policy = load_runner_policy(&state.pool, &auth.runner_id).await?;

    // Serve a pending one-shot restart command, then clear it (serve-and-consume).
    // Same for the capability re-probe command: the Runner sees either on its
    // next heartbeat (≤30s) and acts immediately instead of idling.
    let (restart_requested, reprobe_requested) =
        crate::store::runners::pending_commands(&state.db(), &auth.runner_id).await?;

    if restart_requested {
        tracing::info!(runner = %auth.runner_id, "Serving restart command via heartbeat");
        crate::store::runners::clear_restart_requested(&state.db(), &auth.runner_id).await?;
        log_runner_event(
            &state.pool,
            &auth.runner_id,
            "restart_served",
            "Restart command delivered to runner",
        )
        .await;
    }

    if reprobe_requested {
        tracing::info!(runner = %auth.runner_id, "Serving capability re-probe command via heartbeat");
        crate::store::runners::clear_capability_reprobe_requested(&state.db(), &auth.runner_id)
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
        let (frozen, restarting) =
            crate::store::runners::long_poll_gate(&state.db(), &auth.runner_id).await?;
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
    crate::store::runners::acknowledge_cancelled(&state.db(), &auth.runner_id, cancel_acks).await?;
    let cancelled_jobs =
        crate::store::runners::pending_cancelled_for_delivery(&state.db(), &auth.runner_id).await?;

    // Reject frozen runners — they should not be dispatched work.
    // Also skip dispatch while a restart is pending: otherwise the window between
    // the restart request and the next heartbeat (up to 30s) would let this
    // runner claim pending jobs that then block the graceful exit.
    let (frozen, restart_requested) =
        crate::store::runners::dispatch_gate(&state.db(), &auth.runner_id).await?;

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
    let max_concurrent =
        crate::store::runners::max_concurrent_of(&state.db(), &auth.runner_id).await?;

    let running_count =
        crate::store::runners::running_job_count(&state.db(), &auth.runner_id).await?;

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
    // Load this runner's labels for scheduling.
    let (runner_team_id, runner_labels_raw) =
        crate::store::runners::team_and_labels(&state.db(), &auth.runner_id).await?;
    let runner_labels: Vec<String> = serde_json::from_str(&runner_labels_raw).unwrap_or_default();

    // Slot-aware dispatch context: execution mode, attached sandbox host,
    // queue-walk strategy, and the staleness threshold (see slot_scheduler).
    let dispatch = {
        let row = crate::store::runners::dispatch_context_row(&state.db(), &auth.runner_id).await?;
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
    let candidates =
        crate::store::runners::claim_candidates(&state.pool, runner_team_id.as_deref()).await?;
    let mut claimed = None;
    for row in candidates {
        let job_id: String = row.job_id;
        let snapshot_raw: String = row.snapshot;
        let pipeline_labels_json: String = row.runner_labels;
        let debug_unredact: bool = row.debug_unredact != 0;
        let verbose_logs: bool = row.verbose_logs != 0;
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
                &state.db(),
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
                let _ = crate::store::runners::fail_pending_job(&state.db(), &job_id).await;
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
        let mut tx = crate::store::tx::begin_immediate(&state.pool).await?;
        let Some(candidate) =
            crate::store::runners::claim_candidate_tx(&mut tx, &auth.runner_id, &job_id).await?
        else {
            tx.rollback().await?;
            continue;
        };
        let candidate_pipeline_id: String = candidate.0;
        let candidate_environment_id: Option<String> = candidate.1;
        let candidate_runner_team: Option<String> = candidate.2;
        let candidate_job_team: Option<String> = candidate.3;
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
                        let waited: i64 = row.waited_seconds;
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
            match crate::store::runners::environment_gate_in_tx(
                &mut tx,
                &job_id,
                &candidate_pipeline_id,
                env_id,
            )
            .await?
            {
                crate::store::runners::EnvironmentGate::Blocked => {
                    tx.rollback().await?;
                    continue;
                }
                crate::store::runners::EnvironmentGate::Proceed { superseded } => {
                    superseded_ids = superseded;
                }
            }
        }

        let got = crate::store::runners::claim_job_tx(
            &mut tx,
            &auth.runner_id,
            &job_id,
            slots,
            &resource_spec.to_string(),
            &sandbox_key,
        )
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
                row.trigger_by,
                row.pipeline_name,
                row.team_id,
                row.trigger_type,
            ));
            // `r` carries the six RETURNING columns; the candidate row above
            // carries the pipeline facts the payload also needs.
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
            job_id: row.0,
            pipeline_id: row.1,
            trigger_params_raw: row.2,
            job_environment_id: row.3,
            run_number: row.4,
            run_attempt: row.5,
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
        let defs_raw = crate::store::pipelines::input_defs_of(&state.db(), &pipeline_id)
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
        resolutions_by_order =
            crate::store::pipelines::step_resolution_defs(&state.db(), &pipeline_id)
                .await?
                .into_iter()
                .collect();
    }
    let task_rows = if has_frozen_tasks {
        crate::store::runners::frozen_task_rows(&state.pool, &snapshot_raw).await?
    } else {
        crate::store::runners::live_task_rows(&state.pool, &pipeline_id).await?
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
                &state.db(),
                env_id,
                &state.config.auth.jwt_secret,
            )
            .await?
        } else {
            std::collections::HashMap::new()
        };

    // ── Explicit secret bindings (pipeline_secrets table) ──
    // Same-team only: ignore stale/cross-team rows if any slipped into the table.
    let explicit_rows =
        crate::store::runners::bound_secret_values(&state.db(), &pipeline_id).await?;
    for (name, stored) in explicit_rows {
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
    let team_secret_rows =
        crate::store::runners::team_shared_secret_values(&state.db(), &pipeline_id).await?;
    for (name, stored) in team_secret_rows {
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
    let var_rows = crate::store::runners::bound_variable_values(&state.db(), &pipeline_id).await?;
    for (name, value) in var_rows {
        if let Some(obj) = env.as_object_mut() {
            obj.insert(name, serde_json::Value::String(value));
        }
    }

    // Environment Variable overrides (same keys only — does not inject unbound vars).
    if let Some(ref env_id) = job_environment_id {
        let override_rows =
            crate::store::runners::environment_variable_overrides(&state.db(), env_id).await?;
        let mut applied = 0u32;
        for (name, value) in override_rows {
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
    let env_rows = crate::store::pipelines::inline_envs(&state.db(), &pipeline_id).await?;
    for (name, value) in env_rows {
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
        let exec_type: String = t.exec_type.clone();
        let host_id_str: String = t.host_id.clone();
        let host_names_raw: String = t.host_names.clone();
        let host_labels_raw: String = t.host_labels.clone();
        let step_secrets_str: String = t.step_secrets.clone();
        let step_env_raw: String = t.step_env.clone();
        let image: String = t.image.clone();
        let volumes_raw: String = t.volumes.clone();
        let hosts_strategy: String = t.hosts_strategy.clone();
        let privileged: i64 = t.privileged;
        let capabilities: Vec<String> = serde_json::from_str(&t.capabilities).map_err(|_| {
            AppError::ValidationFailed(format!("task {} has invalid capabilities JSON", t.id))
        })?;
        shared::plugin_spec::validate_plugin_capabilities(&capabilities, &exec_type)
            .map_err(AppError::ValidationFailed)?;

        let mut task_json = serde_json::json!({
            "id": t.id,
            "step_order": t.step_order,
            "name": t.name,
            "exec_type": &exec_type,
            "command_template": t.command_template,
            "work_dir": t.work_dir,
            "retry_count": t.retry_count,
            "timeout_seconds": t.timeout_seconds,
            "fail_strategy": t.fail_strategy,
            "image": &image,
            "volumes": serde_json::from_str::<serde_json::Value>(&volumes_raw)
                .unwrap_or(serde_json::Value::Array(vec![])),
            "hosts_strategy": shared::pipeline_yaml::normalize_hosts_strategy(
                Some(hosts_strategy.as_str()),
            )
            .unwrap_or("chain"),
            "privileged": privileged != 0,
            "capabilities": capabilities,
            "hosts_max_parallel": t.hosts_max_parallel.map(|v| v as u64),
            "needs": serde_json::from_str::<Vec<String>>(&t.needs_json)
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

        if let Some(defs_raw) = resolutions_by_order.get(&t.step_order) {
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
        if let Some(defs_raw) = resolutions_by_order.get(&t.step_order) {
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
                    if let Ok(Some(id)) = crate::store::hosts::visible_id_by_name(
                        &state.db(),
                        name,
                        team_id.as_deref(),
                    )
                    .await
                    {
                        if !host_ids.contains(&id) {
                            host_ids.push(id);
                        }
                    } else {
                        tracing::warn!(
                            "SSH step '{}': resolved dynamic host name '{}' not found",
                            t.name,
                            name
                        );
                    }
                }

                // Look up resolved labels
                for label in &resolved_labels {
                    let rows = crate::store::hosts::visible_ids_by_label(
                        // json_each walk: still SQLite-only (dialect gap), takes the raw pool.
                        &state.pool,
                        label,
                        team_id.as_deref(),
                    )
                    .await
                    .unwrap_or_default();
                    for id in rows {
                        if !host_ids.contains(&id) {
                            host_ids.push(id);
                        }
                    }
                }
            }

            if host_ids.len() == 1 {
                // Single host — return as object (backward-compatible)
                if let Ok(Some(host_row)) = crate::store::hosts::visible_credentials_by_id(
                    &state.db(),
                    &host_ids[0],
                    team_id.as_deref(),
                )
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
                    if let Ok(Some(host_row)) = crate::store::hosts::visible_credentials_by_id(
                        &state.db(),
                        hid,
                        team_id.as_deref(),
                    )
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
    let team_name = match team_id.as_deref() {
        Some(id) => crate::store::teams::name_of(&state.db(), id).await?,
        None => None,
    };

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
    let rows = crate::store::runners::team_secret_values_by_name(
        &DbPool::from_sqlite(pool),
        pipeline_id,
        &needed,
    )
    .await?;
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
fn build_host_json(
    host_row: &crate::store::hosts::HostCredentialsRow,
    jwt_secret: &str,
) -> serde_json::Value {
    let host = host_row.host.clone();
    let port = host_row.port;
    let username = host_row.username.clone();
    let cred_type = host_row.credential_type.clone();
    let cred_data_encrypted = host_row.credential_data.clone();
    let passphrase_encrypted = host_row.credential_passphrase.clone();

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
    let rows = crate::store::runners::list_for_admin(&state.db()).await?;

    let runners: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| -> Result<serde_json::Value, AppError> {
            let system_info = serde_json::from_str::<serde_json::Value>(&r.system_info).ok();
            let policy = RunnerPolicyConfig {
                max_concurrent_jobs: r.max_concurrent_jobs.max(1) as u32,
                clean_workspace_after_job: i64_bool(r.clean_workspace_after_job),
                shell_enabled: i64_bool(r.shell_enabled),
                ssh_enabled: i64_bool(r.ssh_enabled),
                docker_enabled: i64_bool(r.docker_enabled),
                wasm_enabled: i64_bool(r.wasm_enabled),
                ssh_host_key_policy: if r.ssh_host_key_policy == "insecure" {
                    r.ssh_host_key_policy.clone()
                } else {
                    "known_hosts".into()
                },
                execution_mode: parse_execution_mode(r.execution_mode)?,
                claim_strategy: normalize_claim_strategy(Some(r.claim_strategy.clone()))
                    .unwrap_or_else(|_| "backfill".into()),
                netns_isolation: i64_bool(r.netns_isolation),
            };
            Ok(serde_json::json!({
                "id": r.id,
                "name": r.name,
                "labels": serde_json::from_str::<Vec<String>>(&r.labels).unwrap_or_default(),
                "status": r.status,
                "version": r.version,
                "system_info": system_info,
                "last_seen": r.last_seen,
                "is_frozen": r.is_frozen != 0,
                "capability_reprobe_requested": r.capability_reprobe_requested != 0,
                "capability_reprobe_requested_at": r.capability_reprobe_requested_at,
                "capability_reprobe_served_at": r.capability_reprobe_served_at,
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
                "team_id": r.team_id,
                "created_at": r.created_at,
                "updated_at": r.updated_at,
                "running_jobs": r.running_jobs,
                "host_id": r.host_id,
                "runner_instance_id": r.runner_instance_id,
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
    let r = crate::store::runners::detail_of(&state.db(), &id)
        .await?
        .ok_or_else(|| AppError::NotFound("runner not found".into()))?;

    let system_info = serde_json::from_str::<serde_json::Value>(&r.system_info).ok();
    let running_jobs = crate::store::runners::running_jobs_of(&state.db(), &id).await?;
    let policy = load_runner_policy(&state.pool, &id).await?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.id,
        "name": r.name,
        "labels": serde_json::from_str::<Vec<String>>(&r.labels).unwrap_or_default(),
        "status": r.status,
        "version": r.version,
        "system_info": system_info,
        "last_seen": r.last_seen,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
        "team_id": r.team_id,
        "host_id": r.host_id,
        "runner_instance_id": r.runner_instance_id,
        "capability_reprobe_requested": r.capability_reprobe_requested != 0,
        "capability_reprobe_requested_at": r.capability_reprobe_requested_at,
        "capability_reprobe_served_at": r.capability_reprobe_served_at,
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

pub async fn update_runner(
    auth: PlatformAdmin,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateRunnerRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if !crate::store::runners::exists(&state.db(), &id).await? {
        return Err(AppError::NotFound("runner not found".into()));
    }
    // Snapshot before the field-by-field updates so the audit record states
    // the diff, not the whole row.
    let before_policy = load_runner_policy(&state.pool, &id).await?;
    let before_team = crate::store::runners::team_of(&state.db(), &id).await?;
    let before_labels = crate::store::runners::labels_of(&state.db(), &id).await?;
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
        crate::store::runners::move_team(&state.db(), &id, validated.as_deref()).await?;
    }

    if let Some(labels) = &req.labels {
        let labels_json = serde_json::to_string(labels).unwrap_or_else(|_| "[]".into());
        crate::store::runners::update_labels(&state.db(), &id, &labels_json).await?;
    }

    if req.max_concurrent_jobs.is_some() {
        let max_concurrent = normalize_max_concurrent(req.max_concurrent_jobs)?;
        crate::store::runners::update_max_concurrent(&state.db(), &id, max_concurrent as i64)
            .await?;
    }

    if let Some(mode) = req.execution_mode {
        crate::store::runners::change_execution_mode(&state.pool, &id, mode).await?;
    }

    if let Some(v) = req.clean_workspace_after_job {
        crate::store::runners::update_clean_workspace(&state.db(), &id, v).await?;
    }
    if let Some(v) = req.shell_enabled {
        crate::store::runners::update_shell_enabled(&state.db(), &id, v).await?;
    }
    if let Some(v) = req.ssh_enabled {
        crate::store::runners::update_ssh_enabled(&state.db(), &id, v).await?;
    }
    if let Some(v) = req.docker_enabled {
        crate::store::runners::update_docker_enabled(&state.db(), &id, v).await?;
    }
    if let Some(v) = req.wasm_enabled {
        crate::store::runners::update_wasm_enabled(&state.db(), &id, v).await?;
    }
    if let Some(v) = req.netns_isolation {
        crate::store::runners::update_netns_isolation(&state.db(), &id, v).await?;
    }
    if req.ssh_host_key_policy.is_some() {
        let p = normalize_ssh_host_key_policy(req.ssh_host_key_policy.clone())?;
        crate::store::runners::update_ssh_host_key_policy(&state.db(), &id, &p).await?;
    }
    if req.claim_strategy.is_some() {
        let strategy = normalize_claim_strategy(req.claim_strategy.clone())?;
        crate::store::runners::update_claim_strategy(&state.db(), &id, &strategy).await?;
    }

    log_runner_event(&state.pool, &id, "updated", "Runner policy updated from UI").await;

    let r = crate::store::runners::identity_row(&state.db(), &id).await?;

    // `identity_row` is (id, name, labels, status, version, system_info,
    // last_seen, created_at, updated_at, team_id).
    let runner_name = r.1.clone();
    let labels: String = r.2.clone();
    let system_info_raw: String = r.5.clone();
    let team_after: Option<String> = r.9.clone();
    let system_info = serde_json::from_str::<serde_json::Value>(&system_info_raw).ok();
    let running_jobs = crate::store::runners::running_job_count(&state.db(), &id).await?;
    let policy = load_runner_policy(&state.pool, &id).await?;

    // Diff against the snapshot taken before the updates.
    let after = serde_json::json!({
        "team_id": team_after,
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
            .on(Resource::runner(&id, &runner_name))
            .team(team_after.clone())
            .changes(Some(before), Some(after)),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.0,
        "name": runner_name,
        "labels": serde_json::from_str::<Vec<String>>(&labels).unwrap_or_default(),
        "status": r.3,
        "version": r.4,
        "system_info": system_info,
        "last_seen": r.6,
        "created_at": r.7,
        "updated_at": r.8,
        "team_id": team_after.clone(),
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
    let removed = crate::store::runners::runner_snapshot(&state.db(), &id).await?;

    if !crate::store::runners::delete_runner(&state.db(), &id).await? {
        return Err(AppError::NotFound("runner not found".into()));
    }

    if let Some((name, team_id, status)) = removed {
        audit::record(
            &state.pool,
            AuditEvent::new(Action::RunnerDelete, &auth)
                .on(Resource::runner(&id, &name))
                .before(Some(serde_json::json!({
                    "name": name,
                    "team_id": team_id,
                    "status": status,
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
    let (name, was_frozen, now_frozen) =
        crate::store::runners::toggle_freeze(&state.db(), &id).await?;

    let action = if now_frozen { "frozen" } else { "unfrozen" };
    tracing::info!("Runner {name} ({id}) {action}");

    log_runner_event(&state.pool, &id, action, &format!("Runner {action}")).await;
    audit::record(
        &state.pool,
        AuditEvent::new(Action::RunnerFreeze, &auth)
            .on(Resource::runner(&id, &name))
            .changes(
                Some(serde_json::json!({ "is_frozen": was_frozen })),
                Some(serde_json::json!({ "is_frozen": now_frozen })),
            ),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "is_frozen": now_frozen
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
    let state_row = crate::store::runners::runner_restart_state(&state.db(), &id).await?;
    let name = state_row.name;

    if !state_row.running_job_ids.is_empty() {
        return Err(AppError::Conflict(format!(
            "runner has {} running job(s): {} — cancel them before restarting",
            state_row.running_job_ids.len(),
            state_row.running_job_ids.join(", ")
        )));
    }

    crate::store::runners::request_restart(&state.db(), &id).await?;

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
    let name = crate::store::runners::runner_name(&state.db(), &id)
        .await?
        .ok_or_else(|| AppError::NotFound("runner not found".into()))?;

    crate::store::runners::request_capability_reprobe(&state.db(), &id).await?;

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
    // Hash-only at rest: the plaintext is returned exactly once below.
    let name = crate::store::runners::reissue_register_token(
        &state.db(),
        &id,
        &shared::api_token::hash_token(&register_token),
    )
    .await?
    .ok_or_else(|| AppError::NotFound("runner not found".into()))?;

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
    if !crate::store::runners::exists(&state.db(), &id).await? {
        return Err(AppError::NotFound("runner not found".into()));
    }

    let page: i64 = params.get("page").and_then(|p| p.parse().ok()).unwrap_or(1);
    let page_size: i64 = params
        .get("page_size")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    let offset = (page - 1) * page_size;

    let crate::store::runners::RunnerEventPage { events, total } =
        crate::store::runners::event_page(&state.db(), &id, page_size, offset).await?;

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
