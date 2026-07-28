use axum::http::StatusCode;
use axum::{extract::State, response::IntoResponse, Json};
use shared::auth::TokenManager;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateRunnerRequest, HeartbeatRequest, RegisterRequest, RegisterResponse};

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

pub async fn create_runner(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<CreateRunnerRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let id = Uuid::new_v4().to_string();
    let register_token = Uuid::new_v4().to_string();
    let labels_json =
        serde_json::to_string(&req.labels.unwrap_or_default()).unwrap_or_else(|_| "[]".into());

    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, version)
         VALUES (?, ?, ?, ?, 'offline', ?)",
    )
    .bind(&id)
    .bind(&req.name)
    .bind(&register_token)
    .bind(&labels_json)
    .bind(&req.version)
    .execute(&state.pool)
    .await?;

    tracing::info!(
        "Runner created: {} ({id}) register_token={register_token}",
        req.name
    );

    log_runner_event(
        &state.pool,
        &id,
        "created",
        &format!("Runner created, awaiting registration"),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": req.name,
        "register_token": register_token,
    }))))
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

        if existing_token.as_ref().map_or(true, |t| t.is_empty()) {
            // First-time registration — must match the one-shot register_token.
            let stored_rt: String = row.get(1);
            if stored_rt != register_token {
                tracing::warn!(
                    runner = %name,
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

    let labels_json = serde_json::to_string(&req.labels).unwrap_or_else(|_| "[]".into());
    let version = req.version.as_deref().filter(|v| !v.is_empty());

    sqlx::query(
        "UPDATE runners SET token = ?, register_token = '', status = 'online', labels = ?, \
         version = COALESCE(?, version), last_seen = datetime('now'), updated_at = datetime('now') WHERE id = ?",
    )
    .bind(&runner_jwt)
    .bind(&labels_json)
    .bind(version)
    .bind(&runner_id)
    .execute(&state.pool)
    .await?;

    tracing::info!("Runner registered: {} ({runner_id})", name);

    log_runner_event(
        &state.pool,
        &runner_id,
        "registered",
        &format!(
            "Runner registered (version: {}, labels: {:?})",
            req.version.unwrap_or_default(),
            req.labels
        ),
    )
    .await;

    Ok(Json(ApiResponse::success(RegisterResponse {
        runner_id,
        token: runner_jwt,
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

    Ok((StatusCode::OK, Json(ApiResponse::success(()))))
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
        "SELECT j.id, j.pipeline_id, j.trigger_params, COALESCE(j.snapshot, '{}'), p.runner_labels
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
                "Runner claimed job"
            );
            claimed = Some(r);
            break;
        }
        // Lost the race — try next matching candidate
    }

    let Some(row) = claimed else {
        // No matching pending job
        return Ok(Json(ApiResponse::success(None)));
    };

    let job_id: String = row.get(0);
    let pipeline_id: String = row.get(1);
    let trigger_params_raw: Option<String> = row.get(2);
    let trigger_params: serde_json::Value = trigger_params_raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}));

    // Fetch tasks for this pipeline
    let task_rows = sqlx::query(
        "SELECT id, step_order, name, exec_type, host_id, host_names, host_labels, command_template,
                work_dir, retry_count, timeout_seconds, fail_strategy, step_secrets, image
         FROM tasks WHERE pipeline_id = ? ORDER BY step_order",
    )
    .bind(&pipeline_id)
    .fetch_all(&state.pool)
    .await?;

    // Resolve pipeline secrets from explicit bindings
    let mut secrets: serde_json::Value = serde_json::json!({});

    // ── Explicit secret bindings (pipeline_secrets table) ──
    let explicit_rows = sqlx::query(
        "SELECT s.name, s.value
         FROM pipeline_secrets ps
         JOIN secrets s ON s.id = ps.secret_id
         WHERE ps.pipeline_id = ?",
    )
    .bind(&pipeline_id)
    .fetch_all(&state.pool)
    .await?;
    for sr in &explicit_rows {
        let name: String = sr.get(0);
        let value: String = sr.get(1);
        if let Some(obj) = secrets.as_object_mut() {
            obj.insert(name, serde_json::Value::String(value));
        }
    }

    // ── Team auto-access: secrets sharing the pipeline's team_id ──
    let team_secret_rows = sqlx::query(
        "SELECT s.name, s.value
         FROM secrets s
         JOIN pipelines p ON p.team_id = s.team_id AND p.team_id != ''
         WHERE p.id = ?",
    )
    .bind(&pipeline_id)
    .fetch_all(&state.pool)
    .await?;
    for sr in &team_secret_rows {
        let name: String = sr.get(0);
        let value: String = sr.get(1);
        if let Some(obj) = secrets.as_object_mut() {
            obj.entry(name).or_insert(serde_json::Value::String(value));
        }
    }

    // ── Explicit variable bindings (pipeline_vars table) ──
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
        if let Some(obj) = secrets.as_object_mut() {
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
        if let Some(obj) = secrets.as_object_mut() {
            obj.insert(name, serde_json::Value::String(value));
        }
    }

    let mut tasks: Vec<serde_json::Value> = Vec::new();
    for t in &task_rows {
        let exec_type: String = t.get(3);
        let host_id_str: String = t.get(4);
        let host_names_raw: String = t.get(5);
        let host_labels_raw: String = t.get(6);
        let step_secrets_str: String = t.get(12);
        let image: String = t.get(13);

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
            "image": image,
        });

        // Resolve step-level secrets (only for this step).
        // Step secrets may come from the step_secrets column (by name, stored at pipeline
        // creation time) and need to be resolved from the secrets table at dispatch time.
        let mut step_secrets_map = serde_json::json!({});
        if !step_secrets_str.is_empty() {
            let step_names: Vec<&str> = step_secrets_str
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            if !step_names.is_empty() {
                // Query the secrets table directly for step-level secrets
                let placeholders: Vec<String> = step_names
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", i + 1))
                    .collect();
                let query = format!(
                    "SELECT name, value FROM secrets WHERE name IN ({})",
                    placeholders.join(",")
                );
                let mut q = sqlx::query_as::<_, (String, String)>(&query);
                for name in &step_names {
                    q = q.bind(*name);
                }
                if let Ok(rows) = q.fetch_all(&state.pool).await {
                    for (name, value) in rows {
                        if let Some(obj) = step_secrets_map.as_object_mut() {
                            obj.insert(name, serde_json::Value::String(value));
                        }
                    }
                }
            }
        }

        // For SSH steps, resolve host IDs → connection details
        if exec_type == "ssh" {
            let mut host_ids: Vec<String> = host_id_str
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            // ── Dynamic host resolution: resolve variable references ──
            // Merge trigger_params + secrets into a unified lookup (inputs take priority)
            let mut unified_vars = serde_json::json!({});
            if let Some(obj) = secrets.as_object() {
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
                    .map(|s| resolve_var_ref(s.trim(), &unified_vars))
                    .flat_map(|resolved| resolved)
                    .collect();

                let resolved_labels: Vec<String> = host_labels_raw
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| resolve_var_ref(s.trim(), &unified_vars))
                    .flat_map(|resolved| resolved)
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

        // Attach step-level secrets
        task_json["step_secrets"] = step_secrets_map;

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

    Ok(Json(ApiResponse::success(Some(serde_json::json!({
        "job_id": job_id,
        "pipeline_id": pipeline_id,
        "tasks": tasks,
        "inputs": trigger_params,
        "secrets": secrets,
    })))))
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
        "SELECT id, name, labels, status, version, system_info, last_seen, is_frozen, created_at, updated_at FROM runners ORDER BY created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;

    let runners: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let labels: String = r.get(2);
            let system_info_raw: String = r.get(5);
            let system_info = serde_json::from_str::<serde_json::Value>(&system_info_raw).ok();
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "name": r.get::<String, _>(1),
                "labels": serde_json::from_str::<Vec<String>>(&labels).unwrap_or_default(),
                "status": r.get::<String, _>(3),
                "version": r.get::<Option<String>, _>(4),
                "system_info": system_info,
                "last_seen": r.get::<Option<String>, _>(6),
                "is_frozen": r.get::<i64, _>(7) != 0,
                "created_at": r.get::<Option<String>, _>(8),
                "updated_at": r.get::<Option<String>, _>(9),
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
        "SELECT id, name, labels, status, version, system_info, last_seen, register_token, created_at, updated_at FROM runners WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("runner not found".into()))?;

    let labels: String = r.get(2);
    let system_info_raw: String = r.get(5);
    let system_info = serde_json::from_str::<serde_json::Value>(&system_info_raw).ok();
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
    }))))
}

pub async fn delete_runner(
    _auth: PlatformAdmin,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
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
