//! Integration tests for the pre-retirement deprecated-key scan
//! (`services::deprecated_scan`) — Phase 3 precondition ①.

mod common;

use manager::store::exec;

use manager::services::deprecated_scan::scan_pool;

async fn test_pool() -> exec::DbPool {
    let pool = common::test_db(4).await;
    pool
}

fn unique_tmp_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "buz-scan-test-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).expect("create tmp dir");
    dir
}

#[tokio::test]
async fn scan_reports_tasks_secrets_and_files() {
    let pool = test_pool().await;

    exec::execute(
        &pool,
        "INSERT INTO pipelines (id, name, source) VALUES ('p1', 'demo-app', 'form')",
        &[],
    )
    .await
    .unwrap();
    exec::execute(&pool,
        "INSERT INTO tasks (id, pipeline_id, step_order, name, command_template, registry_secret, config_source, config_key_path)
         VALUES ('t1', 'p1', 1, 'deploy', 'docker compose up -d', 'ACR_PERSONAL', 'oss://example-bucket/cfg.tar.gz', 'oss-cfg')",
        &[],
    )
    .await
    .unwrap();
    exec::execute(
        &pool,
        "INSERT INTO tasks (id, pipeline_id, step_order, name, command_template)
         VALUES ('t2', 'p1', 2, 'clean', 'docker system prune -f')",
        &[],
    )
    .await
    .unwrap();

    for (id, name, kind) in [
        ("s1", "ACR_PERSONAL", "registry"),
        ("s2", "OSS_READONLY", "aliyun_oss_auth"),
        ("s3", "POSTGRES_PASSWORD", "generic"),
    ] {
        exec::execute(
            &pool,
            "INSERT INTO secrets (id, name, value, owner_group, kind) VALUES (?, ?, 'x', '', ?)",
            &[id.into(), name.into(), kind.into()],
        )
        .await
        .unwrap();
    }

    let dir = unique_tmp_dir("affected");
    std::fs::write(
        dir.join("legacy.yml"),
        "name: legacy-deploy\nsteps:\n  - name: deploy\n    engine: docker\n    image: registry.example.com/foo/app:1\n    registry_secret: ACR_PERSONAL\n    config:\n      source: oss://example-bucket/configs.tar.gz\n      key_path: oss-cfg\n      dest: config\n    run: echo hi\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("clean.yml"),
        "name: clean-deploy\nsteps:\n  - name: go\n    engine: shell\n    run: echo hi\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("broken.yml"),
        "name: broken\nbogus_key: true\nsteps: []\n",
    )
    .unwrap();

    let report = scan_pool(&pool.clone(), Some(&dir)).await;

    assert_eq!(report.task_findings.len(), 1, "one affected pipeline task");
    let task = &report.task_findings[0];
    assert_eq!(task.location, "demo-app / step \"deploy\"");
    for key in ["registry_secret", "config.source", "config.key_path"] {
        assert!(
            task.keys.iter().any(|k| k == key),
            "task finding missing {key}: {:?}",
            task.keys
        );
    }

    assert_eq!(report.secret_findings.len(), 2, "two retired-kind secrets");
    for name in ["ACR_PERSONAL", "OSS_READONLY"] {
        assert!(report.secret_findings.iter().any(|f| f.location == name));
    }

    // `registry_secret` is no longer a known YAML key — the strict parser
    // rejects the file outright (rejection semantics), so legacy.yml shows up
    // as unreadable instead of a file finding.
    assert_eq!(
        report.file_findings.len(),
        0,
        "no parseable YAML file findings: {:?}",
        report.file_findings
    );

    assert_eq!(
        report.files_unreadable.len(),
        2,
        "legacy.yml rejected + broken.yml reported: {:?}",
        report.files_unreadable
    );
    assert!(
        report
            .files_unreadable
            .iter()
            .any(|e| e.contains("legacy.yml") && e.contains("registry_secret")),
        "legacy.yml rejected for the retired key: {:?}",
        report.files_unreadable
    );
    assert!(
        report
            .files_unreadable
            .iter()
            .any(|e| e.contains("broken.yml")),
        "broken.yml reported: {:?}",
        report.files_unreadable
    );

    assert!(!report.is_empty());
    let mut out = Vec::new();
    report.print(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Summary: 1 pipeline(s), 0 file(s), 2 secret(s) affected."));
    assert!(text.contains("buz/docker-login"));

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn scan_clean_install_is_empty() {
    let pool = test_pool().await;
    let dir = unique_tmp_dir("clean");
    std::fs::write(
        dir.join("ok.yml"),
        "name: ok\nsteps:\n  - name: go\n    engine: shell\n    run: echo hi\n",
    )
    .unwrap();

    let report = scan_pool(&pool.clone(), Some(&dir)).await;
    assert!(report.is_empty(), "clean install must scan empty");

    std::fs::remove_dir_all(&dir).ok();
}
