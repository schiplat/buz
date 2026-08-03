use axum::http::StatusCode;
use axum::{extract::State, response::IntoResponse, Json};
use shared::auth::TokenManager;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{
    CreateRunnerRequest, HeartbeatRequest, HeartbeatResponse, RegisterRequest, RegisterResponse,
    RunnerPolicyConfig, UpdateRunnerRequest,
};

use crate::crypto;
use crate::middleware::{PlatformAdmin, RunnerClaims};
use crate::state::AppState;
use sqlx::Row;
use uuid::Uuid;

/// True when the runner has **every** label required by the pipeline/job
/// (intersection / "must match ALL"). Empty `required` matches any runner.
pub(crate) fn runner_satisfies_labels(runner_labels: &[String], required: &[String]) -> bool {
    required
        .iter()
        .all(|need| runner_labels.iter().any(|have| have == need))
}

/// Prefer frozen job snapshot `runner_labels`; fall back to live pipeline labels JSON.
fn required_labels_for_job(snapshot_json: &str, pipeline_labels_json: &str) -> Vec<String> {
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
pub(crate) fn normalize_max_concurrent(v: Option<u32>) -> Result<u32, AppError> {
    let n = v.unwrap_or(1);
    if !(1..=32).contains(&n) {
        return Err(AppError::ValidationFailed(
            "max_concurrent_jobs must be between 1 and 32".into(),
        ));
    }
    Ok(n)
}

pub(crate) fn normalize_ssh_host_key_policy(v: Option<String>) -> Result<String, AppError> {
    let p = v.unwrap_or_else(|| "insecure".into());
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

/// Load UI policy columns for a runner.
async fn load_runner_policy(
    pool: &sqlx::SqlitePool,
    runner_id: &str,
) -> Result<RunnerPolicyConfig, AppError> {
    let row = sqlx::query(
        "SELECT max_concurrent_jobs, clean_workspace_after_job,
                shell_enabled, ssh_enabled, docker_enabled, wasm_enabled, ssh_host_key_policy
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
            if p == "known_hosts" {
                p
            } else {
                "insecure".into()
            }
        },
    })
}

fn policy_json(p: &RunnerPolicyConfig) -> serde_json::Value {
    serde_json::json!({
        "max_concurrent_jobs": p.max_concurrent_jobs,
        "clean_workspace_after_job": p.clean_workspace_after_job,
        "shell_enabled": p.shell_enabled,
        "ssh_enabled": p.ssh_enabled,
        "docker_enabled": p.docker_enabled,
        "wasm_enabled": p.wasm_enabled,
        "ssh_host_key_policy": p.ssh_host_key_policy,
    })
}

pub async fn create_runner(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<CreateRunnerRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let created = create_runner_record(&state.pool, &req).await?;

    let policy = load_runner_policy(&state.pool, &created.id).await?;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": created.id,
        "name": created.name,
        "register_token": created.register_token,
        "max_concurrent_jobs": created.max_concurrent_jobs,
        "policy": policy_json(&policy),
    }))))
}

/// Core runner creation used by both the HTTP handler and the
/// `gen-runner-token` CLI subcommand. Persists the runner and returns its
/// one-shot register token.
pub(crate) async fn create_runner_record(
    pool: &sqlx::SqlitePool,
    req: &CreateRunnerRequest,
) -> Result<CreatedRunner, AppError> {
    let id = Uuid::new_v4().to_string();
    let register_token = Uuid::new_v4().to_string();
    let labels_json = serde_json::to_string(req.labels.as_deref().unwrap_or_default())
        .unwrap_or_else(|_| "[]".into());
    let max_concurrent = normalize_max_concurrent(req.max_concurrent_jobs)?;
    let ssh_policy = normalize_ssh_host_key_policy(req.ssh_host_key_policy.clone())?;
    let clean = bool_i64(req.clean_workspace_after_job, false);
    let shell = bool_i64(req.shell_enabled, true);
    let ssh = bool_i64(req.ssh_enabled, true);
    let docker = bool_i64(req.docker_enabled, false);
    let wasm = bool_i64(req.wasm_enabled, false);

    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, version,
         max_concurrent_jobs, clean_workspace_after_job,
         shell_enabled, ssh_enabled, docker_enabled, wasm_enabled, ssh_host_key_policy)
         VALUES (?, ?, ?, ?, 'offline', ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&req.name)
    .bind(&register_token)
    .bind(&labels_json)
    .bind(&req.version)
    .bind(max_concurrent as i64)
    .bind(clean)
    .bind(shell)
    .bind(ssh)
    .bind(docker)
    .bind(wasm)
    .bind(&ssh_policy)
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
pub(crate) struct CreatedRunner {
    pub id: String,
    pub name: String,
    pub register_token: String,
    pub max_concurrent_jobs: u32,
}

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<ApiResponse<RegisterResponse>>, AppError> {
    let name = req.name.trim();
    let register_token = req.register_token.trim();

    // Look up the runner by name first.
    let existing = sqlx::query("SELECT id, register_token, token FROM runners WHERE name = ?")
        .bind(name)
        .fetch_optional(&state.pool)
        .await?;

    let runner_id: String;

    if let Some(ref row) = existing {
        let existing_token: Option<String> = row.get(2);

        if existing_token.as_ref().is_none_or(|t| t.is_empty()) {
            // First-time registration — must match the one-shot register_token.
            let stored_rt: String = row.get(1);
            if stored_rt != register_token {
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
        // else: re-registration — runner already has a JWT; allow without re-checking register_token.

        runner_id = row.get(0);
    } else {
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

    sqlx::query(
        "UPDATE runners SET token = ?, register_token = '', status = 'online', \
         version = COALESCE(?, version), last_seen = datetime('now'), updated_at = datetime('now') WHERE id = ?",
    )
    .bind(&runner_jwt)
    .bind(version)
    .bind(&runner_id)
    .execute(&state.pool)
    .await?;

    tracing::info!("Runner registered: {} ({runner_id})", name);

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

    sqlx::query(
        "UPDATE runners SET status = ?, system_info = ?, version = COALESCE(?, version), \
         last_seen = datetime('now'), updated_at = datetime('now') WHERE id = ?",
    )
    .bind(&req.status)
    .bind(&system_info_json)
    .bind(version)
    .bind(&auth.runner_id)
    .execute(&state.pool)
    .await?;

    let policy = load_runner_policy(&state.pool, &auth.runner_id).await?;

    Ok((
        StatusCode::OK,
        Json(ApiResponse::success(HeartbeatResponse { policy })),
    ))
}

pub async fn poll(
    auth: RunnerClaims,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Option<serde_json::Value>>>, AppError> {
    // Reject frozen runners — they should not be dispatched work
    let frozen: bool = sqlx::query_scalar("SELECT is_frozen FROM runners WHERE id = ?")
        .bind(&auth.runner_id)
        .fetch_one(&state.pool)
        .await
        .map(|v: i64| v != 0)
        .unwrap_or(false);

    if frozen {
        tracing::debug!("Runner {} is frozen — skipping poll", auth.runner_id);
        return Ok(Json(ApiResponse::success(None)));
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

    if running_count >= max_concurrent {
        tracing::debug!(
            runner = %auth.runner_id,
            running = running_count,
            max = max_concurrent,
            "Runner at capacity — skipping poll"
        );
        return Ok(Json(ApiResponse::success(None)));
    }

    // Load this runner's labels for scheduling.
    let runner_labels: Vec<String> =
        sqlx::query_scalar::<_, String>("SELECT labels FROM runners WHERE id = ?")
            .bind(&auth.runner_id)
            .fetch_optional(&state.pool)
            .await?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();

    // Candidates: oldest pending jobs with pipeline (+ snapshot) labels.
    // Claim only if runner has ALL required labels (see runner_satisfies_labels).
    let candidates = sqlx::query(
        "SELECT j.id, j.pipeline_id, j.trigger_params, COALESCE(j.snapshot, '{}'), p.runner_labels,
                j.debug_unredact, j.verbose_logs
         FROM jobs j
         JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.status = 'pending'
         ORDER BY j.created_at ASC
         LIMIT 50",
    )
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

        let got = sqlx::query(
            "UPDATE jobs SET status = 'running', runner_id = ?, start_time = datetime('now'),
             updated_at = datetime('now')
             WHERE id = ? AND status = 'pending'
             RETURNING id, pipeline_id, trigger_params",
        )
        .bind(&auth.runner_id)
        .bind(&job_id)
        .fetch_optional(&state.pool)
        .await?;

        if let Some(r) = got {
            tracing::info!(
                runner = %auth.runner_id,
                job = %job_id,
                runner_labels = ?runner_labels,
                required_labels = ?required,
                debug_unredact,
                verbose_logs,
                "Runner claimed job"
            );
            claimed = Some((r, snapshot_raw, debug_unredact, verbose_logs));
            break;
        }
        // Lost the race — try next matching candidate
    }

    let Some((row, snapshot_raw, debug_unredact, verbose_logs)) = claimed else {
        // No matching pending job
        return Ok(Json(ApiResponse::success(None)));
    };

    let job_id: String = row.get(0);
    let pipeline_id: String = row.get(1);
    let trigger_params_raw: Option<String> = row.get(2);
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
    // Fetch tasks for this pipeline
    let task_rows = sqlx::query(
        "SELECT id, step_order, name, exec_type, host_id, host_names, host_labels, command_template,
                work_dir, retry_count, timeout_seconds, fail_strategy, step_secrets, step_env, image, volumes,
                registry_secret, config_source, config_key_path, config_render, config_dest
         FROM tasks WHERE pipeline_id = ? ORDER BY step_order",
    )
    .bind(&pipeline_id)
    .fetch_all(&state.pool)
    .await?;

    // Resolve pipeline secrets from explicit bindings + team_shared (sensitive).
    // `secrets:` / team_shared are an **allowlist** for `${secrets.NAME}` —
    // values are NOT auto-injected as `$NAME`. Call sites must map explicitly:
    //   env:
    //     GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}
    // or purpose slots: `registry_secret: ${secrets.ACR_PERSONAL}`.
    //
    // Claim still sends `secrets` (name→value) for log masking + config.render.
    let mut secrets: serde_json::Value = serde_json::json!({});
    let mut secrets_by_name: serde_json::Value = serde_json::json!({});
    let mut env: serde_json::Value = serde_json::json!({});
    let mut secret_env_keys: Vec<String> = Vec::new();

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
            Ok(value) => {
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
            Ok(value) => {
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
        let registry_secret_raw: String = t.get(16);
        let config_source_raw: String = t.get(17);
        let config_key_path_raw: String = t.get(18);
        let config_render_raw: String = t.get(19);
        let config_dest: String = t.get(20);

        // Purpose slots accept `${secrets.NAME}` or bare NAME.
        let registry_secret_name =
            shared::secret_ref::resolve_secret_slot(&registry_secret_raw).unwrap_or_default();
        let config_source_name =
            shared::secret_ref::resolve_secret_slot(&config_source_raw).unwrap_or_default();

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
            "registry_secret": &registry_secret_name,
            "config_source": &config_source_name,
            "config_key_path": &config_key_path_raw,
            "config_render": serde_json::from_str::<serde_json::Value>(&config_render_raw)
                .unwrap_or(serde_json::Value::Null),
            "config_dest": &config_dest,
        });

        // Values that must be redacted in the Runner's logs. Registered here — the
        // single source of truth for "which values are sensitive" (see
        // `shared::log_masking::SecretClass`). Adding a new sensitive field means
        // pushing its value here; the Runner only consumes this list.
        let mut log_mask_values: Vec<String> = Vec::new();

        if !registry_secret_raw.trim().is_empty() && registry_secret_name.is_empty() {
            task_json["registry_auth_error"] = serde_json::Value::String(format!(
                "invalid registry_secret '{registry_secret_raw}' — use ${{secrets.NAME}} or a bare secret name"
            ));
        }
        if !config_source_raw.trim().is_empty() && config_source_name.is_empty() {
            task_json["config_auth_error"] = serde_json::Value::String(format!(
                "invalid config.source '{config_source_raw}' — use ${{secrets.NAME}} or a bare secret name"
            ));
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
            )
            .await?;
        }

        // Implicit allowlist from purpose slots + step env `${secrets.X}` (top-level optional).
        {
            let mut implied: Vec<String> = Vec::new();
            if !registry_secret_name.trim().is_empty() {
                implied.push(registry_secret_name.trim().to_string());
            }
            if !config_source_name.trim().is_empty() {
                implied.push(config_source_name.trim().to_string());
            }
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
                    if let Ok(Some(row)) = sqlx::query("SELECT id FROM hosts WHERE name = ?")
                        .bind(name)
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
                        "SELECT id FROM hosts, json_each(hosts.labels) WHERE json_each.value = ?",
                    )
                    .bind(label)
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
                     FROM hosts WHERE id = ?",
                )
                .bind(&host_ids[0])
                .fetch_optional(&state.pool)
                .await
                {
                    task_json["host"] = build_host_json(&host_row, &state.config.auth.jwt_secret);
                }
            } else if host_ids.len() > 1 {
                // Multiple hosts — return as array
                let mut host_list: Vec<serde_json::Value> = Vec::new();
                for hid in &host_ids {
                    if let Ok(Some(host_row)) = sqlx::query(
                        "SELECT host, port, username, credential_type, credential_data
                         FROM hosts WHERE id = ?",
                    )
                    .bind(hid)
                    .fetch_optional(&state.pool)
                    .await
                    {
                        host_list.push(build_host_json(&host_row, &state.config.auth.jwt_secret));
                    }
                }
                task_json["host"] = serde_json::Value::Array(host_list);
            } else {
                // No hosts resolved from the database
                let task_name = task_json["name"].as_str().unwrap_or("unnamed");
                tracing::warn!(
                    "SSH step '{task_name}': no hosts resolved (static host_id='{}', raw names='{}', raw labels='{}')",
                    host_id_str, host_names_raw, host_labels_raw
                );
                task_json["host"] = serde_json::Value::Array(vec![]);
            }
        }

        // Resolve registry_secret → registry_auth (purpose slot — not env inject).
        // Same-team + kind=registry; name must be allowlisted (pipeline/step/team_shared).
        if !registry_secret_name.trim().is_empty() && task_json.get("registry_auth_error").is_none()
        {
            let allowed = secrets_by_name.get(registry_secret_name.trim()).is_some()
                || step_secrets_map.get(registry_secret_name.trim()).is_some();
            if !allowed {
                task_json["registry_auth_error"] = serde_json::Value::String(format!(
                    "registry_secret '{}' not found for this pipeline's team — create the secret \
                     (same team), or enable Team share; reference via registry_secret: ${{secrets.{}}}",
                    registry_secret_name.trim(),
                    registry_secret_name.trim()
                ));
            }
        }
        if !registry_secret_name.trim().is_empty() && task_json.get("registry_auth_error").is_none()
        {
            match sqlx::query_as::<_, (String, String)>(
                "SELECT s.value, s.kind FROM secrets s
                 JOIN pipelines p ON p.team_id = s.team_id AND p.team_id != ''
                 WHERE p.id = ? AND s.name = ?",
            )
            .bind(&pipeline_id)
            .bind(registry_secret_name.trim())
            .fetch_optional(&state.pool)
            .await
            {
                Ok(Some((stored, kind))) => {
                    if !shared::secret_kind::is_registry_kind(&kind) {
                        task_json["registry_auth_error"] = serde_json::Value::String(format!(
                            "secret '{}' has kind='{}' — set Kind to Registry credentials \
                             in Infrastructure → Secrets (or create a registry secret)",
                            registry_secret_name.trim(),
                            shared::secret_kind::normalize_secret_kind(Some(&kind))
                        ));
                    } else {
                        match crypto::decrypt(&stored, &state.config.auth.jwt_secret) {
                            Ok(value) => {
                                let image_opt = if image.trim().is_empty() {
                                    None
                                } else {
                                    Some(image.as_str())
                                };
                                match shared::registry_auth::parse_registry_secret_value(
                                    &value, image_opt,
                                ) {
                                    Ok(auth) => {
                                        tracing::info!(
                                            secret = %registry_secret_name.trim(),
                                            registry = %auth.registry,
                                            "Resolved registry_secret → registry_auth for job dispatch"
                                        );
                                        let mut auth_json = serde_json::json!({
                                            "registry": auth.registry,
                                            "username": auth.username,
                                            "password": auth.password,
                                        });
                                        if let Some(p) = auth.provider {
                                            auth_json["provider"] = serde_json::Value::String(p);
                                        }
                                        task_json["registry_auth"] = auth_json;
                                        // SecretClass::RegistryPassword
                                        log_mask_values.push(auth.password.clone());
                                    }
                                    Err(e) => {
                                        task_json["registry_auth_error"] =
                                            serde_json::Value::String(e);
                                    }
                                }
                            }
                            Err(e) => {
                                task_json["registry_auth_error"] = serde_json::Value::String(
                                    format!("failed to decrypt registry_secret: {e}"),
                                );
                            }
                        }
                    }
                }
                Ok(None) => {
                    task_json["registry_auth_error"] = serde_json::Value::String(format!(
                        "registry_secret '{}' not found for this pipeline's team (create it under the same team, or check the name)",
                        registry_secret_name.trim()
                    ));
                }
                Err(e) => {
                    task_json["registry_auth_error"] = serde_json::Value::String(format!(
                        "failed to load registry_secret '{}': {e}",
                        registry_secret_name.trim()
                    ));
                }
            }
        }

        // Resolve config_source → signed OSS GET URL (injected as $BUZ_CONFIG_URL).
        // Same-team + kind=aliyun_oss_auth; name must be allowlisted.
        if !config_source_name.trim().is_empty() && task_json.get("config_auth_error").is_none() {
            let allowed = secrets_by_name.get(config_source_name.trim()).is_some()
                || step_secrets_map.get(config_source_name.trim()).is_some();
            if !allowed {
                task_json["config_auth_error"] = serde_json::Value::String(format!(
                    "config.source '{}' not found for this pipeline's team — create an OSS secret \
                     (same team), or enable Team share; reference via source: ${{secrets.{}}}",
                    config_source_name.trim(),
                    config_source_name.trim()
                ));
            }
        }

        // Attach step allowlist values for render/mask (Runner does not auto-inject `$NAME`).
        task_json["step_secrets"] = step_secrets_map.clone();

        if !config_source_name.trim().is_empty() && task_json.get("config_auth_error").is_none() {
            match sqlx::query_as::<_, (String, String)>(
                "SELECT s.value, s.kind FROM secrets s
                 JOIN pipelines p ON p.team_id = s.team_id AND p.team_id != ''
                 WHERE p.id = ? AND s.name = ?",
            )
            .bind(&pipeline_id)
            .bind(config_source_name.trim())
            .fetch_optional(&state.pool)
            .await
            {
                Ok(Some((stored, kind))) => {
                    if !shared::secret_kind::is_aliyun_oss_auth_kind(&kind) {
                        task_json["config_auth_error"] = serde_json::Value::String(format!(
                            "config_source '{}' has kind='{}' — set Kind to Aliyun OSS auth \
                             in Infrastructure → Secrets (or create an aliyun_oss_auth secret)",
                            config_source_name.trim(),
                            shared::secret_kind::normalize_secret_kind(Some(&kind))
                        ));
                    } else {
                        // Expand `$VAR` / `${VAR}` in config_key_path (Lenient).
                        let mut config_vars = serde_json::json!({});
                        if let Some(obj) = env.as_object() {
                            for (k, v) in obj {
                                config_vars[k] = v.clone();
                            }
                        }
                        if let Some(obj) = secrets_by_name.as_object() {
                            for (k, v) in obj {
                                config_vars[k] = v.clone();
                            }
                        }
                        if let Some(obj) = trigger_params.as_object() {
                            for (k, v) in obj {
                                config_vars[k] = v.clone();
                            }
                        }
                        let config_map = shared::value_source::ValueMap {
                            vars: config_vars
                                .as_object()
                                .map(|o| {
                                    o.iter()
                                        .filter_map(|(k, v)| {
                                            v.as_str().map(|s| (k.clone(), s.to_string()))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                            secrets: std::collections::HashMap::new(),
                        };
                        let expanded_path = shared::value_source::expand(
                            &config_key_path_raw,
                            &config_map,
                            shared::value_source::ExpandMode::Lenient,
                            shared::value_source::MissingPolicy::Blank,
                        )
                        .map(|e| e.content)
                        .unwrap_or_else(|_| config_key_path_raw.clone());
                        let object_key = {
                            let dir = expanded_path.trim().trim_matches('/');
                            if dir.is_empty() {
                                None
                            } else {
                                Some(format!("{dir}/configs.tar.gz"))
                            }
                        };
                        match object_key {
                            None => {
                                task_json["config_auth_error"] = serde_json::Value::String(
                                    "config_source requires a non-empty 'config_key_path' (e.g. config_tpl/super-manager/v1.4.0)"
                                        .into(),
                                );
                            }
                            Some(key) if key.contains('$') => {
                                task_json["config_auth_error"] = serde_json::Value::String(
                                    format!(
                                        "config_key_path '{config_key_path_raw}' left unresolved variable(s) '${}' — inputs are: {}",
                                        key.split('$').nth(1).unwrap_or(""),
                                        trigger_params
                                            .as_object()
                                            .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
                                            .unwrap_or_default()
                                    ),
                                );
                            }
                            Some(key) => {
                                match crypto::decrypt(&stored, &state.config.auth.jwt_secret) {
                                    Ok(value) => {
                                        match shared::config_auth::parse_oss_secret_value(&value) {
                                            Ok(auth) => {
                                                let ttl =
                                            shared::config_auth::DEFAULT_CONFIG_URL_TTL_SECS;
                                                match shared::config_auth::sign_oss_get_url(
                                                    &auth, &key, ttl,
                                                ) {
                                                    Ok(url) => {
                                                        tracing::info!(
                                                            secret = %config_source_name.trim(),
                                                            object_key = %key,
                                                            ttl_secs = ttl,
                                                            "Resolved config_source → signed OSS URL for job dispatch"
                                                        );
                                                        task_json["config_auth"] = serde_json::json!({
                                                            "url": url,
                                                            "object_key": key,
                                                            "expires_in_secs": ttl,
                                                        });
                                                        // SecretClass::ConfigUrl
                                                        log_mask_values.push(url.clone());
                                                    }
                                                    Err(e) => {
                                                        task_json["config_auth_error"] =
                                                            serde_json::Value::String(e);
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                task_json["config_auth_error"] =
                                                    serde_json::Value::String(e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        task_json["config_auth_error"] = serde_json::Value::String(
                                            format!("failed to decrypt config_source: {e}"),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(None) => {
                    task_json["config_auth_error"] = serde_json::Value::String(format!(
                        "config_source '{}' not found for this pipeline's team (create an OSS secret under the same team, or check the name)",
                        config_source_name.trim()
                    ));
                }
                Err(e) => {
                    task_json["config_auth_error"] = serde_json::Value::String(format!(
                        "failed to load config_source '{}': {e}",
                        config_source_name.trim()
                    ));
                }
            }
        }

        // `config_render` resolves `${secrets.X}` by secret name — attach for
        // any step that opts into render (pipeline allowlist + step allowlist).
        if !config_render_raw.is_empty() {
            let mut render_secrets = secrets_by_name.clone();
            if let (Some(dst), Some(src)) =
                (render_secrets.as_object_mut(), step_secrets_map.as_object())
            {
                for (k, v) in src {
                    dst.entry(k.clone()).or_insert(v.clone());
                }
            }
            task_json["render_env_secrets"] = render_secrets;
        }

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

    Ok(Json(ApiResponse::success(Some(serde_json::json!({
        "job_id": job_id,
        "pipeline_id": pipeline_id,
        "tasks": tasks,
        "inputs": trigger_params,
        "masking_input_keys": masking_input_keys,
        "env": env,
        "secret_env_keys": secret_env_keys,
        "secrets": secrets,
        "debug_unredact": debug_unredact,
        "verbose": verbose_logs,
    })))))
}

/// Load same-team secrets by name into `into_by_name` (and optionally a parallel map).
/// Skips names already present. Used for GHA-style implicit allowlist via `${secrets.X}`
/// / purpose slots when top-level `secrets:` is omitted.
async fn merge_team_secrets_by_name(
    pool: &sqlx::SqlitePool,
    pipeline_id: &str,
    jwt_secret: &str,
    names: &[String],
    into_by_name: &mut serde_json::Value,
    mut into_parallel: Option<&mut serde_json::Value>,
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
            Ok(value) => {
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
                    .insert(format!("INPUT_{}", k.to_uppercase()), s.to_string());
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
                r.created_at, r.updated_at,
                (SELECT COUNT(*) FROM jobs j WHERE j.runner_id = r.id AND j.status = 'running') AS running_jobs
         FROM runners r ORDER BY r.created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;

    let runners: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
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
                    if p == "known_hosts" {
                        p
                    } else {
                        "insecure".into()
                    }
                },
            };
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "name": r.get::<String, _>(1),
                "labels": serde_json::from_str::<Vec<String>>(&labels).unwrap_or_default(),
                "status": r.get::<String, _>(3),
                "version": r.get::<Option<String>, _>(4),
                "system_info": system_info,
                "last_seen": r.get::<Option<String>, _>(6),
                "is_frozen": r.get::<i64, _>(7) != 0,
                "max_concurrent_jobs": policy.max_concurrent_jobs,
                "clean_workspace_after_job": policy.clean_workspace_after_job,
                "shell_enabled": policy.shell_enabled,
                "ssh_enabled": policy.ssh_enabled,
                "docker_enabled": policy.docker_enabled,
                "wasm_enabled": policy.wasm_enabled,
                "ssh_host_key_policy": policy.ssh_host_key_policy,
                "created_at": r.get::<Option<String>, _>(15),
                "updated_at": r.get::<Option<String>, _>(16),
                "running_jobs": r.get::<i64, _>(17),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(runners)))
}

pub async fn get_runner(
    _auth: PlatformAdmin,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let r = sqlx::query(
        "SELECT id, name, labels, status, version, system_info, last_seen, register_token,
                created_at, updated_at FROM runners WHERE id = ?",
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
        "register_token": r.get::<Option<String>, _>(7),
        "created_at": r.get::<Option<String>, _>(8),
        "updated_at": r.get::<Option<String>, _>(9),
        "running_jobs": running_jobs,
        "max_concurrent_jobs": policy.max_concurrent_jobs,
        "clean_workspace_after_job": policy.clean_workspace_after_job,
        "shell_enabled": policy.shell_enabled,
        "ssh_enabled": policy.ssh_enabled,
        "docker_enabled": policy.docker_enabled,
        "wasm_enabled": policy.wasm_enabled,
        "ssh_host_key_policy": policy.ssh_host_key_policy,
    }))))
}

pub async fn update_runner(
    _auth: PlatformAdmin,
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

    log_runner_event(&state.pool, &id, "updated", "Runner policy updated from UI").await;

    let r = sqlx::query(
        "SELECT id, name, labels, status, version, system_info, last_seen, register_token,
                created_at, updated_at FROM runners WHERE id = ?",
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

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.get::<String, _>(0),
        "name": r.get::<String, _>(1),
        "labels": serde_json::from_str::<Vec<String>>(&labels).unwrap_or_default(),
        "status": r.get::<String, _>(3),
        "version": r.get::<Option<String>, _>(4),
        "system_info": system_info,
        "last_seen": r.get::<Option<String>, _>(6),
        "register_token": r.get::<Option<String>, _>(7),
        "created_at": r.get::<Option<String>, _>(8),
        "updated_at": r.get::<Option<String>, _>(9),
        "running_jobs": running_jobs,
        "max_concurrent_jobs": policy.max_concurrent_jobs,
        "clean_workspace_after_job": policy.clean_workspace_after_job,
        "shell_enabled": policy.shell_enabled,
        "ssh_enabled": policy.ssh_enabled,
        "docker_enabled": policy.docker_enabled,
        "wasm_enabled": policy.wasm_enabled,
        "ssh_host_key_policy": policy.ssh_host_key_policy,
    }))))
}

pub async fn delete_runner(
    _auth: PlatformAdmin,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
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

    tracing::info!("Runner deleted: {id}");
    Ok(Json(ApiResponse::success(())))
}

pub async fn toggle_freeze(
    _auth: PlatformAdmin,
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

    let action = if new_frozen != 0 {
        "frozen"
    } else {
        "unfrozen"
    };
    let name: String = row.get(0);
    tracing::info!("Runner {name} ({id}) {action}");

    log_runner_event(&state.pool, &id, action, &format!("Runner {action}")).await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "is_frozen": new_frozen != 0
    }))))
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

#[cfg(test)]
#[path = "tests/runner_label_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/runner_config_path_tests.rs"]
mod config_path_tests;

#[cfg(test)]
#[path = "tests/runner_create_tests.rs"]
mod create_tests;
