use super::create_runner_record;
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
        CREATE TABLE IF NOT EXISTS runners (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            register_token TEXT NOT NULL,
            token TEXT,
            labels TEXT NOT NULL DEFAULT '[]',
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
            ssh_host_key_policy TEXT NOT NULL DEFAULT 'insecure',
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
async fn create_runner_record_returns_register_token() {
    let pool = test_pool().await;
    let req = CreateRunnerRequest {
        name: "cli-runner-1".into(),
        labels: Some(vec!["linux-x64".into(), "docker".into()]),
        version: Some("1.3.8".into()),
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
