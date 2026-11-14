mod common;

use manager::api::runners::create_runner_record;
use manager::store::exec;
use shared::protocol::CreateRunnerRequest;

async fn test_pool() -> exec::DbPool {
    let pool = common::test_db(4).await;
    // The migrated schema replaces the hand-built mini-DDL.
    pool
}

#[tokio::test]
async fn create_runner_record_defaults_execution_mode_to_host() {
    let pool = test_pool().await;
    let req = CreateRunnerRequest {
        name: "host-runner".into(),
        labels: None,
        team_id: None,
        execution_mode: None,
        version: None,
        max_concurrent_jobs: None,
        clean_workspace_after_job: None,
        shell_enabled: None,
        ssh_enabled: None,
        docker_enabled: None,
        wasm_enabled: None,
        ssh_host_key_policy: None,
        claim_strategy: None,
    };
    let created = create_runner_record(&pool.clone(), &req).await.unwrap();
    let mode: String = exec::fetch_optional(
        &pool,
        "SELECT execution_mode FROM runners WHERE id = ?",
        &[created.id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .text(0);
    assert_eq!(mode, "host");
}

#[tokio::test]
async fn create_runner_record_rejects_non_host_execution_mode() {
    use shared::protocol::RunnerExecutionMode;
    let pool = test_pool().await;
    let req = CreateRunnerRequest {
        name: "premature-container-runner".into(),
        labels: None,
        team_id: None,
        execution_mode: Some(RunnerExecutionMode::Container),
        version: None,
        max_concurrent_jobs: None,
        clean_workspace_after_job: None,
        shell_enabled: None,
        ssh_enabled: None,
        docker_enabled: None,
        wasm_enabled: None,
        ssh_host_key_policy: None,
        claim_strategy: None,
    };
    let error = match create_runner_record(&pool.clone(), &req).await {
        Ok(_) => panic!("Container mode must wait for reported host capability"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("start in Host mode"));
}

#[tokio::test]
async fn create_runner_record_binds_valid_team() {
    let pool = test_pool().await;
    exec::execute(
        &pool,
        "INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Alpha', 'alpha')",
        &[],
    )
    .await
    .unwrap();
    let req = CreateRunnerRequest {
        name: "team-runner".into(),
        labels: Some(vec!["linux-x64".into()]),
        team_id: Some("team-1".into()),
        execution_mode: None,
        version: None,
        max_concurrent_jobs: None,
        clean_workspace_after_job: None,
        shell_enabled: None,
        ssh_enabled: None,
        docker_enabled: None,
        wasm_enabled: None,
        ssh_host_key_policy: None,
        claim_strategy: None,
    };

    let created = create_runner_record(&pool.clone(), &req)
        .await
        .expect("create bound runner");
    let team_id: Option<String> = match exec::fetch_optional(
        &pool,
        "SELECT team_id FROM runners WHERE id = ?",
        &[created.id.into()],
    )
    .await
    .unwrap()
    {
        Some(r) => r.opt_text(0),
        None => None,
    };
    assert_eq!(team_id.as_deref(), Some("team-1"));
}

#[tokio::test]
async fn create_runner_record_rejects_unknown_team() {
    let pool = test_pool().await;
    let req = CreateRunnerRequest {
        name: "bad-team-runner".into(),
        labels: None,
        team_id: Some("missing-team".into()),
        execution_mode: None,
        version: None,
        max_concurrent_jobs: None,
        clean_workspace_after_job: None,
        shell_enabled: None,
        ssh_enabled: None,
        docker_enabled: None,
        wasm_enabled: None,
        ssh_host_key_policy: None,
        claim_strategy: None,
    };

    let error = match create_runner_record(&pool.clone(), &req).await {
        Ok(_) => panic!("unknown team must be rejected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("does not exist"));
}

#[tokio::test]
async fn create_runner_record_returns_register_token() {
    let pool = test_pool().await;
    let req = CreateRunnerRequest {
        name: "cli-runner-1".into(),
        labels: Some(vec!["linux-x64".into(), "docker".into()]),
        team_id: None,
        execution_mode: None,
        version: Some("1.5.6".into()),
        max_concurrent_jobs: None,
        clean_workspace_after_job: None,
        shell_enabled: None,
        ssh_enabled: None,
        docker_enabled: None,
        wasm_enabled: None,
        ssh_host_key_policy: None,
        claim_strategy: None,
    };

    let created = create_runner_record(&pool.clone(), &req)
        .await
        .expect("create");
    assert_eq!(created.name, "cli-runner-1");
    assert!(!created.register_token.is_empty());
    assert_eq!(created.max_concurrent_jobs, 1);
    assert!(!created.id.is_empty());

    // Row is persisted with labels + policy defaults.
    let (token, labels): (String, String) = {
        let r = exec::fetch_optional(
            &pool,
            "SELECT register_token, labels FROM runners WHERE name = ?",
            &["cli-runner-1".into()],
        )
        .await
        .unwrap()
        .expect("fetch runner");
        (r.text(0), r.text(1))
    };
    // Hash-only at rest: the row carries SHA-256(token), never the plaintext.
    assert_eq!(
        token,
        shared::api_token::hash_token(&created.register_token)
    );
    assert!(labels.contains("linux-x64"));
    assert!(labels.contains("docker"));
}

#[tokio::test]
async fn create_runner_record_rejects_duplicate_name() {
    let pool = test_pool().await;
    let req = CreateRunnerRequest {
        name: "dup-runner".into(),
        labels: None,
        team_id: None,
        execution_mode: None,
        version: None,
        max_concurrent_jobs: None,
        clean_workspace_after_job: None,
        shell_enabled: None,
        ssh_enabled: None,
        docker_enabled: None,
        wasm_enabled: None,
        ssh_host_key_policy: None,
        claim_strategy: None,
    };

    create_runner_record(&pool.clone(), &req)
        .await
        .expect("first create");
    let second = create_runner_record(&pool.clone(), &req).await;
    assert!(
        second.is_err(),
        "duplicate name must fail on UNIQUE constraint"
    );
}
