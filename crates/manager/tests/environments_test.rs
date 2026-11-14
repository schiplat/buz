//! Integration tests for platform Environments: CRUD, trigger resolution,
//! and claim-time Variable override merge order.

mod common;

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use manager::store::exec;
use shared::auth::{Role, TokenManager};

use tower::ServiceExt;

use manager::api;
use manager::config::AuthConfig;
use manager::config::Config;
use manager::config::PipelineConfig;
use manager::config::RunnerConfig;
use manager::config::ServerConfig;
use manager::services::log_broadcast::LogBroadcaster;
use manager::state::AppState;

/// Form retirement: create/update write a YAML file under `yaml_scan_dir`.
/// Tests in one binary run in parallel, so each gets its own directory (leaked
/// on purpose — the process exits after the test binary finishes).
fn isolated_pipeline_config() -> PipelineConfig {
    let path = tempfile::tempdir().expect("temp scan dir").keep();
    PipelineConfig {
        yaml_scan_dir: path.to_string_lossy().into_owned(),
        ..PipelineConfig::default()
    }
}

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const SUPER_ADMIN_ID: &str = "admin-1";

fn test_config() -> Config {
    Config {
        server: ServerConfig::default(),
        database: Default::default(),
        cluster: Default::default(),
        auth: AuthConfig {
            jwt_secret: JWT_SECRET.into(),
            jwt_ttl_hours: 24,
            runner_register_token_ttl_hours: 24,
        },
        runner: RunnerConfig::default(),
        pipeline: isolated_pipeline_config(),
        standalone: Default::default(),
        plugin_index: Default::default(),
        storage: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
    }
}

async fn test_app() -> (Router, exec::DbPool) {
    let pool = common::test_db(4).await;

    exec::execute(&pool,
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 1, 1, 1)",
        &[SUPER_ADMIN_ID.into(), "boss".into(), "boss".into()],
    )
    .await
    .expect("seed admin");

    exec::execute(
        &pool,
        "INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')",
        &[],
    )
    .await
    .expect("seed team");

    exec::execute(
        &pool,
        "INSERT INTO runners (id, name, register_token, labels, status, max_concurrent_jobs)
         VALUES ('runner-1', 'r1', 'tok', '[\"linux-x64\"]', 'online', 2)",
        &[],
    )
    .await
    .expect("seed runner");

    let state = AppState {
        pool: pool.clone(),
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    (Router::new().nest("/api/v1", api::router(state)), pool)
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn admin_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(SUPER_ADMIN_ID, Role::Admin, 1)
        .expect("mint admin jwt")
}

fn runner_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token("runner-1", Role::Runner, 1)
        .expect("mint runner jwt")
}

async fn read_body(response: axum::response::Response) -> serde_json::Value {
    let body = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    serde_json::from_slice(&body).expect("json body")
}

async fn json_req(
    app: &Router,
    method: &str,
    uri: &str,
    jwt: &str,
    body: Option<serde_json::Value>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(AUTHORIZATION, bearer(jwt));
    let req_body = if let Some(b) = body {
        builder = builder.header("Content-Type", "application/json");
        Body::from(serde_json::to_vec(&b).unwrap())
    } else {
        Body::empty()
    };
    app.clone()
        .oneshot(builder.body(req_body).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn environment_create_and_update_reject_surrounding_slug_whitespace() {
    let (app, _pool) = test_app().await;
    let jwt = admin_jwt();

    for slug in [" production", "production ", "\tproduction"] {
        let response = json_req(
            &app,
            "POST",
            "/api/v1/environments",
            &jwt,
            Some(serde_json::json!({
                "name": format!("Bad {slug:?}"),
                "slug": slug,
            })),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "slug={slug:?}"
        );
    }

    let created = json_req(
        &app,
        "POST",
        "/api/v1/environments",
        &jwt,
        Some(serde_json::json!({ "name": "Production", "slug": "production" })),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let env_id = read_body(created).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    for slug in [" production", "production "] {
        let response = json_req(
            &app,
            "PUT",
            &format!("/api/v1/environments/{env_id}"),
            &jwt,
            Some(serde_json::json!({ "slug": slug })),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "slug={slug:?}"
        );
    }
}

#[tokio::test]
async fn environment_crud_and_override_roundtrip() {
    let (app, _pool) = test_app().await;
    let jwt = admin_jwt();

    let create = json_req(
        &app,
        "POST",
        "/api/v1/environments",
        &jwt,
        Some(serde_json::json!({
            "name": "Production",
            "slug": "production",
            "description": "prod overlays",
        })),
    )
    .await;
    assert_eq!(create.status(), StatusCode::OK, "create env");
    let created = read_body(create).await;
    let env_id = created["data"]["id"].as_str().expect("env id").to_string();
    assert_eq!(created["data"]["slug"], "production");

    let var_resp = json_req(
        &app,
        "POST",
        "/api/v1/variables",
        &jwt,
        Some(serde_json::json!({
            "name": "ACR_REPOSITORY",
            "value": "myteam/default",
        })),
    )
    .await;
    assert_eq!(var_resp.status(), StatusCode::OK, "create var");
    let var_body = read_body(var_resp).await;
    let var_id = var_body["data"]["id"].as_str().expect("var id").to_string();

    let upsert = json_req(
        &app,
        "PUT",
        &format!("/api/v1/environments/{env_id}/overrides"),
        &jwt,
        Some(serde_json::json!({
            "var_id": var_id,
            "value": "myteam/prod",
        })),
    )
    .await;
    assert_eq!(upsert.status(), StatusCode::OK, "upsert override");

    let detail = json_req(
        &app,
        "GET",
        &format!("/api/v1/environments/{env_id}"),
        &jwt,
        None,
    )
    .await;
    assert_eq!(detail.status(), StatusCode::OK);
    let d = read_body(detail).await;
    let overrides = d["data"]["overrides"].as_array().expect("overrides");
    assert_eq!(overrides.len(), 1);
    assert_eq!(overrides[0]["value"], "myteam/prod");
    assert_eq!(overrides[0]["var_name"], "ACR_REPOSITORY");

    let clear = json_req(
        &app,
        "DELETE",
        &format!("/api/v1/environments/{env_id}/overrides/{var_id}"),
        &jwt,
        None,
    )
    .await;
    assert_eq!(clear.status(), StatusCode::OK, "clear override");

    let list = json_req(&app, "GET", "/api/v1/environments", &jwt, None).await;
    assert_eq!(list.status(), StatusCode::OK);
    let listed = read_body(list).await;
    assert_eq!(listed["data"].as_array().unwrap().len(), 2);
    let production = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|environment| environment["slug"] == "production")
        .expect("production environment");
    assert_eq!(production["override_count"], 0);
}

#[tokio::test]
async fn trigger_undeclared_environment_is_rejected() {
    let (app, _pool) = test_app().await;
    let jwt = admin_jwt();

    let mut pipeline_body = serde_json::json!({
        "name": "env-404",
        "trigger_type": "manual",
        "runner_labels": ["linux-x64"],
        "team_id": "team-1",
        "environments": ["default"],
        "env": [],
        "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
    });
    let pipe = json_req(
        &app,
        "POST",
        "/api/v1/pipelines",
        &jwt,
        Some(pipeline_body.take()),
    )
    .await;
    assert_eq!(pipe.status(), StatusCode::OK);
    let pid = read_body(pipe).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pid}/trigger"),
        &jwt,
        Some(serde_json::json!({
            "inputs": {},
            "environment": "no-such-env",
        })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_body(resp).await;
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("not in this pipeline"));
}

#[tokio::test]
async fn restricted_environment_rejects_dynamic_host_targets() {
    let (app, pool) = test_app().await;
    let jwt = admin_jwt();

    exec::execute(
        &pool,
        "INSERT INTO hosts (id, name, host, port, username, credential_type, credential_data)
         VALUES ('host-prod', 'prod-01', '10.0.0.1', 22, 'root', 'none', '{}')",
        &[],
    )
    .await
    .expect("seed host");

    let environment = json_req(
        &app,
        "POST",
        "/api/v1/environments",
        &jwt,
        Some(serde_json::json!({ "name": "Production", "slug": "production" })),
    )
    .await;
    let env_id = read_body(environment).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let allow_host = json_req(
        &app,
        "POST",
        &format!("/api/v1/environments/{env_id}/host-allowlist"),
        &jwt,
        Some(serde_json::json!({ "kind": "name", "value": "allowed-01" })),
    )
    .await;
    assert_eq!(allow_host.status(), StatusCode::OK);

    let pipeline = json_req(
        &app,
        "POST",
        "/api/v1/pipelines",
        &jwt,
        Some(serde_json::json!({
            "name": "dynamic-host",
            "trigger_type": "manual",
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "environments": ["production"],
            "input_defs": [{ "name": "target", "required": true }],
            "env": [],
            "steps": [{
                "name": "deploy",
                "run": "echo deploy",
                "engine": "ssh",
                "host": "$target"
            }]
        })),
    )
    .await;
    assert_eq!(pipeline.status(), StatusCode::OK, "create pipeline");
    let pipeline_id = read_body(pipeline).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let trigger = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipeline_id}/trigger"),
        &jwt,
        Some(serde_json::json!({
            "inputs": { "target": "prod-01" },
            "environment": "production"
        })),
    )
    .await;
    assert_eq!(trigger.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_body(trigger).await;
    assert!(body["message"].as_str().unwrap().contains("dynamic host"));
    let job_count: i64 = exec::fetch_optional(&pool, "SELECT COUNT(*) FROM jobs", &[])
        .await
        .unwrap()
        .unwrap()
        .i64(0);
    assert_eq!(job_count, 0, "rejected host must not create a job");
}

#[tokio::test]
async fn claim_applies_environment_overrides_bound_keys_only() {
    let (app, pool) = test_app().await;
    let jwt = admin_jwt();

    // Bound var (will be overridden) + unbound var (override must not inject).
    let bound = json_req(
        &app,
        "POST",
        "/api/v1/variables",
        &jwt,
        Some(serde_json::json!({
            "name": "ACR_REPOSITORY",
            "value": "myteam/default",
        })),
    )
    .await;
    let bound_id = read_body(bound).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let unbound = json_req(
        &app,
        "POST",
        "/api/v1/variables",
        &jwt,
        Some(serde_json::json!({
            "name": "UNBOUND_ONLY",
            "value": "should-not-appear",
        })),
    )
    .await;
    let unbound_id = read_body(unbound).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let env_create = json_req(
        &app,
        "POST",
        "/api/v1/environments",
        &jwt,
        Some(serde_json::json!({
            "name": "Staging",
            "slug": "staging",
        })),
    )
    .await;
    let env_id = read_body(env_create).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    for (vid, val) in [(&bound_id, "myteam/staging"), (&unbound_id, "injected-bad")] {
        let r = json_req(
            &app,
            "PUT",
            &format!("/api/v1/environments/{env_id}/overrides"),
            &jwt,
            Some(serde_json::json!({ "var_id": vid, "value": val })),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
    }

    // Pipeline binds only ACR_REPOSITORY; inline env wins last for INLINE_KEY.
    let pipe = json_req(
        &app,
        "POST",
        "/api/v1/pipelines",
        &jwt,
        Some(serde_json::json!({
            "name": "env-claim",
            "trigger_type": "manual",
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "environments": ["staging"],
            "var_ids": [bound_id],
            "env": [{ "name": "INLINE_KEY", "value": "from-yaml" }],
            "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
        })),
    )
    .await;
    assert_eq!(pipe.status(), StatusCode::OK, "create pipeline");
    let pid = read_body(pipe).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let trig = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pid}/trigger"),
        &jwt,
        Some(serde_json::json!({
            "inputs": {},
            "environment": "staging",
        })),
    )
    .await;
    assert_eq!(trig.status(), StatusCode::OK, "trigger with staging");
    let trig_body = read_body(trig).await;
    let job_id = trig_body["data"]["job_id"].as_str().expect("job_id");

    let job_env_id: Option<String> = match exec::fetch_optional(
        &pool,
        "SELECT environment_id FROM jobs WHERE id = ?",
        &[job_id.into()],
    )
    .await
    .unwrap()
    {
        Some(r) => r.opt_text(0),
        None => None,
    };
    assert_eq!(job_env_id.as_deref(), Some(env_id.as_str()));

    let snap_raw: String = exec::fetch_optional(
        &pool,
        "SELECT snapshot FROM jobs WHERE id = ?",
        &[job_id.into()],
    )
    .await
    .unwrap()
    .expect("snapshot")
    .text(0);
    let snap: serde_json::Value = serde_json::from_str(&snap_raw).unwrap();
    assert_eq!(snap["environment"], "staging");

    let poll = json_req(&app, "GET", "/api/v1/runners/poll", &runner_jwt(), None).await;
    assert_eq!(poll.status(), StatusCode::OK);
    let dispatch = read_body(poll).await;
    let env = &dispatch["data"]["env"];
    assert_eq!(
        env["ACR_REPOSITORY"], "myteam/staging",
        "bound var overridden by environment: {env}"
    );
    assert_eq!(
        env["INLINE_KEY"], "from-yaml",
        "pipeline env: still present: {env}"
    );
    assert!(
        env.get("UNBOUND_ONLY").is_none(),
        "unbound override must not inject: {env}"
    );
}

#[tokio::test]
async fn trigger_without_environment_is_rejected() {
    let (app, _pool) = test_app().await;
    let jwt = admin_jwt();

    let var = json_req(
        &app,
        "POST",
        "/api/v1/variables",
        &jwt,
        Some(serde_json::json!({
            "name": "REGION",
            "value": "default-region",
        })),
    )
    .await;
    let var_id = read_body(var).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let env_create = json_req(
        &app,
        "POST",
        "/api/v1/environments",
        &jwt,
        Some(serde_json::json!({ "name": "Prod", "slug": "production" })),
    )
    .await;
    let env_id = read_body(env_create).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = json_req(
        &app,
        "PUT",
        &format!("/api/v1/environments/{env_id}/overrides"),
        &jwt,
        Some(serde_json::json!({ "var_id": var_id, "value": "prod-region" })),
    )
    .await;

    let pipe = json_req(
        &app,
        "POST",
        "/api/v1/pipelines",
        &jwt,
        Some(serde_json::json!({
            "name": "no-env-trigger",
            "trigger_type": "manual",
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "environments": ["production"],
            "var_ids": [var_id],
            "env": [],
            "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
        })),
    )
    .await;
    assert_eq!(pipe.status(), StatusCode::OK, "create pipeline");
    let pid = read_body(pipe).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let trig = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pid}/trigger"),
        &jwt,
        Some(serde_json::json!({ "inputs": {} })),
    )
    .await;
    assert_eq!(trig.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let message = read_body(trig).await["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("environment is required"), "{message}");
}

#[tokio::test]
async fn secret_override_roundtrip_never_returns_plaintext() {
    let (app, _pool) = test_app().await;
    let jwt = admin_jwt();

    let env_create = json_req(
        &app,
        "POST",
        "/api/v1/environments",
        &jwt,
        Some(serde_json::json!({ "name": "Prod", "slug": "production" })),
    )
    .await;
    let env_id = read_body(env_create).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let sec = json_req(
        &app,
        "POST",
        "/api/v1/secrets",
        &jwt,
        Some(serde_json::json!({
            "name": "API_TOKEN",
            "value": "default-token",
            "team_id": "team-1",
        })),
    )
    .await;
    assert_eq!(sec.status(), StatusCode::OK);
    let secret_id = read_body(sec).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let upsert = json_req(
        &app,
        "PUT",
        &format!("/api/v1/environments/{env_id}/secret-overrides"),
        &jwt,
        Some(serde_json::json!({
            "secret_id": secret_id,
            "value": "prod-token-secret",
        })),
    )
    .await;
    assert_eq!(upsert.status(), StatusCode::OK);
    let up = read_body(upsert).await;
    assert_eq!(up["data"]["has_override"], true);
    assert!(up["data"].get("value").is_none() || up["data"]["value"].is_null());
    assert_ne!(
        up["data"]
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "prod-token-secret"
    );

    let detail = json_req(
        &app,
        "GET",
        &format!("/api/v1/environments/{env_id}"),
        &jwt,
        None,
    )
    .await;
    let d = read_body(detail).await;
    let sos = d["data"]["secret_overrides"]
        .as_array()
        .expect("secret_overrides");
    assert_eq!(sos.len(), 1);
    assert_eq!(sos[0]["secret_name"], "API_TOKEN");
    assert_eq!(sos[0]["has_override"], true);
    assert!(sos[0].get("value").is_none() || sos[0]["value"].is_null());

    let list = json_req(&app, "GET", "/api/v1/environments", &jwt, None).await;
    let listed = read_body(list).await;
    assert_eq!(listed["data"][0]["secret_override_count"], 0);
    let production = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|environment| environment["slug"] == "production")
        .expect("production environment");
    assert_eq!(production["secret_override_count"], 1);
}

#[tokio::test]
async fn claim_applies_environment_secret_overrides_allowlisted_only() {
    let (app, _pool) = test_app().await;
    let jwt = admin_jwt();

    let bound = json_req(
        &app,
        "POST",
        "/api/v1/secrets",
        &jwt,
        Some(serde_json::json!({
            "name": "DEPLOY_TOKEN",
            "value": "default-deploy",
            "team_id": "team-1",
        })),
    )
    .await;
    let bound_id = read_body(bound).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let unbound = json_req(
        &app,
        "POST",
        "/api/v1/secrets",
        &jwt,
        Some(serde_json::json!({
            "name": "OTHER_SECRET",
            "value": "should-not-appear",
            "team_id": "team-1",
        })),
    )
    .await;
    let unbound_id = read_body(unbound).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let env_create = json_req(
        &app,
        "POST",
        "/api/v1/environments",
        &jwt,
        Some(serde_json::json!({ "name": "Staging", "slug": "staging" })),
    )
    .await;
    let env_id = read_body(env_create).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    for (sid, val) in [(&bound_id, "staging-deploy"), (&unbound_id, "injected-bad")] {
        let r = json_req(
            &app,
            "PUT",
            &format!("/api/v1/environments/{env_id}/secret-overrides"),
            &jwt,
            Some(serde_json::json!({ "secret_id": sid, "value": val })),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
    }

    let pipe = json_req(
        &app,
        "POST",
        "/api/v1/pipelines",
        &jwt,
        Some(serde_json::json!({
            "name": "secret-env-claim",
            "trigger_type": "manual",
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "environments": ["staging"],
            "secret_ids": [bound_id],
            "env": [{ "name": "TOKEN", "value": "${secrets.DEPLOY_TOKEN}" }],
            "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
        })),
    )
    .await;
    assert_eq!(pipe.status(), StatusCode::OK, "create pipeline");
    let pid = read_body(pipe).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let trig = json_req(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pid}/trigger"),
        &jwt,
        Some(serde_json::json!({
            "inputs": {},
            "environment": "staging",
        })),
    )
    .await;
    assert_eq!(trig.status(), StatusCode::OK);

    let poll = json_req(&app, "GET", "/api/v1/runners/poll", &runner_jwt(), None).await;
    assert_eq!(poll.status(), StatusCode::OK);
    let dispatch = read_body(poll).await;
    let secrets = &dispatch["data"]["secrets"];
    assert_eq!(
        secrets["DEPLOY_TOKEN"], "staging-deploy",
        "allowlisted secret overridden: {secrets}"
    );
    assert!(
        secrets.get("OTHER_SECRET").is_none(),
        "unbound secret override must not inject: {secrets}"
    );
    assert_eq!(
        dispatch["data"]["env"]["TOKEN"], "staging-deploy",
        "env expansion uses override: {}",
        dispatch["data"]["env"]
    );
}
