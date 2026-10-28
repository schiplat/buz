use manager::api::runners::create_runner_record;
use shared::protocol::CreateRunnerRequest;
use sqlx::sqlite::SqlitePoolOptions;

async fn test_pool() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    // Minimal schema needed by create_runner_record (runners + runner_events).
    let sql = r#"
        CREATE TABLE IF NOT EXISTS teams (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE
        );
        CREATE TABLE IF NOT EXISTS runners (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            register_token TEXT NOT NULL,
            token TEXT,
            labels TEXT NOT NULL DEFAULT '[]',
            team_id TEXT,
            execution_mode TEXT NOT NULL DEFAULT 'host',
            status TEXT NOT NULL DEFAULT 'offline',
            version TEXT,
            system_info TEXT NOT NULL DEFAULT '{}',
            is_frozen INTEGER NOT NULL DEFAULT 0,
            max_concurrent_jobs INTEGER NOT NULL DEFAULT 1,
            clean_workspace_after_job INTEGER NOT NULL DEFAULT 0,
            shell_enabled INTEGER NOT NULL DEFAULT 1,
            ssh_enabled INTEGER NOT NULL DEFAULT 1,
            docker_enabled INTEGER NOT NULL DEFAULT 0,
            wasm_enabled INTEGER NOT NULL DEFAULT 0,
            ssh_host_key_policy TEXT NOT NULL DEFAULT 'known_hosts',
            last_seen TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS runner_events (
            id TEXT PRIMARY KEY,
            runner_id TEXT NOT NULL,
            event_type TEXT NOT NULL,
            message TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
    "#;
    for stmt in sql.split(';') {
        let trimmed = stmt.trim();
        if !trimmed.is_empty() {
            sqlx::query(trimmed)
                .execute(&pool)
                .await
                .expect("apply schema");
        }
    }
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
    };
    let created = create_runner_record(&pool, &req).await.unwrap();
    let mode: String = sqlx::query_scalar("SELECT execution_mode FROM runners WHERE id = ?")
        .bind(created.id)
        .fetch_one(&pool)
        .await
        .unwrap();
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
    };
    let error = match create_runner_record(&pool, &req).await {
        Ok(_) => panic!("Container mode must wait for reported host capability"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("start in Host mode"));
}

#[tokio::test]
async fn create_runner_record_binds_valid_team() {
    let pool = test_pool().await;
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Alpha', 'alpha')")
        .execute(&pool)
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
    };

    let created = create_runner_record(&pool, &req)
        .await
        .expect("create bound runner");
    let team_id: Option<String> = sqlx::query_scalar("SELECT team_id FROM runners WHERE id = ?")
        .bind(created.id)
        .fetch_one(&pool)
        .await
        .unwrap();
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
    };

    let error = match create_runner_record(&pool, &req).await {
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
    };

    let created = create_runner_record(&pool, &req).await.expect("create");
    assert_eq!(created.name, "cli-runner-1");
    assert!(!created.register_token.is_empty());
    assert_eq!(created.max_concurrent_jobs, 1);
    assert!(!created.id.is_empty());

    // Row is persisted with labels + policy defaults.
    let (token, labels): (String, String) =
        sqlx::query_as("SELECT register_token, labels FROM runners WHERE name = ?")
            .bind("cli-runner-1")
            .fetch_one(&pool)
            .await
            .expect("fetch runner");
    assert_eq!(token, created.register_token);
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
    };

    create_runner_record(&pool, &req)
        .await
        .expect("first create");
    let second = create_runner_record(&pool, &req).await;
    assert!(
        second.is_err(),
        "duplicate name must fail on UNIQUE constraint"
    );
}
