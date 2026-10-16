//! Real Docker daemon e2e (ignored by default).
//!
//! ```bash
//! BUZ_DOCKER_E2E=1 cargo test -p runner docker_e2e -- --ignored --nocapture
//! ./scripts/docker-engine-e2e.sh
//! ```

use super::{build_docker_run_args, execute_streaming, preflight, CONTAINER_WORKSPACE};
use std::collections::HashMap;
use std::time::Duration;
use tempfile::tempdir;
use tokio::process::Command;
use tokio::time::timeout;

fn docker_e2e_enabled() -> bool {
    std::env::var("BUZ_DOCKER_E2E").ok().as_deref() == Some("1")
}

async fn docker_available() -> bool {
    Command::new("docker")
        .args(["info"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn drain_logs(mut rx: tokio::sync::mpsc::UnboundedReceiver<String>) -> String {
    let mut out = String::new();
    while let Some(chunk) = rx.recv().await {
        out.push_str(&chunk);
    }
    out
}

async fn run_and_collect(
    image: &str,
    runner_workspace_dir: &std::path::Path,
    command: &str,
    env: &HashMap<String, String>,
    container_work_dir: Option<&str>,
    privileged: bool,
) -> anyhow::Result<(i32, String)> {
    let (mut child, rx, guard) = execute_streaming(
        image,
        runner_workspace_dir,
        command,
        env,
        &[],
        container_work_dir,
        privileged,
        None,
    )
    .await?;
    let status = child.wait().await?;
    drop(guard);
    let logs = drain_logs(rx).await;
    let code = status.code().unwrap_or(-1);
    Ok((code, logs))
}

fn require_e2e() {
    assert!(
        docker_e2e_enabled(),
        "set BUZ_DOCKER_E2E=1 to run Docker e2e tests"
    );
}

#[tokio::test]
#[ignore = "requires Docker; set BUZ_DOCKER_E2E=1"]
async fn docker_e2e_preflight_and_daemon() {
    require_e2e();
    assert!(
        docker_available().await,
        "docker info failed — is the daemon running?"
    );
    preflight(true, "alpine:3.20").unwrap();
}

#[tokio::test]
#[ignore = "requires Docker; set BUZ_DOCKER_E2E=1"]
async fn docker_e2e_runs_inside_alpine_not_host() {
    require_e2e();
    assert!(docker_available().await);

    let dir = tempdir().unwrap();
    let (code, logs) = run_and_collect(
        "alpine:3.20",
        dir.path(),
        "cat /etc/os-release && echo BUZ_MARKER_OK",
        &HashMap::new(),
        None,
        false,
    )
    .await
    .expect("docker run");

    assert_eq!(code, 0, "logs:\n{logs}");
    assert!(
        logs.contains("Alpine Linux") || logs.to_lowercase().contains("alpine"),
        "expected Alpine identity in logs:\n{logs}"
    );
    assert!(logs.contains("BUZ_MARKER_OK"), "logs:\n{logs}");
}

#[tokio::test]
#[ignore = "requires Docker; set BUZ_DOCKER_E2E=1"]
async fn docker_e2e_workspace_mount_and_cwd() {
    require_e2e();
    assert!(docker_available().await);

    let dir = tempdir().unwrap();
    let (code, logs) = run_and_collect(
        "alpine:3.20",
        dir.path(),
        &format!(
            "pwd && echo from-container > {CONTAINER_WORKSPACE}/smoke.txt && cat {CONTAINER_WORKSPACE}/smoke.txt"
        ),
        &HashMap::new(),
        None,
        false,
    )
    .await
    .expect("docker run");

    assert_eq!(code, 0, "logs:\n{logs}");
    assert!(
        logs.contains(CONTAINER_WORKSPACE),
        "cwd should be workspace:\n{logs}"
    );
    let host_file = dir.path().join("smoke.txt");
    let content = std::fs::read_to_string(&host_file).expect("host file written by container");
    assert!(content.contains("from-container"), "{content}");
}

#[tokio::test]
#[ignore = "requires Docker; set BUZ_DOCKER_E2E=1"]
async fn docker_e2e_env_injection() {
    require_e2e();
    assert!(docker_available().await);

    let dir = tempdir().unwrap();
    let mut env = HashMap::new();
    env.insert("BUZ_E2E_VAR".into(), "hello-from-env".into());

    let (code, logs) = run_and_collect(
        "alpine:3.20",
        dir.path(),
        "echo \"VAR=$BUZ_E2E_VAR INPUT=$INPUT_BUZ_E2E_VAR\"",
        &env,
        None,
        false,
    )
    .await
    .expect("docker run");

    assert_eq!(code, 0, "logs:\n{logs}");
    assert!(logs.contains("VAR=hello-from-env"), "logs:\n{logs}");
    assert!(logs.contains("INPUT=hello-from-env"), "logs:\n{logs}");
}

#[tokio::test]
#[ignore = "requires Docker; set BUZ_DOCKER_E2E=1"]
async fn docker_e2e_nonzero_exit_fails() {
    require_e2e();
    assert!(docker_available().await);

    let dir = tempdir().unwrap();
    let (code, logs) = run_and_collect(
        "alpine:3.20",
        dir.path(),
        "exit 42",
        &HashMap::new(),
        None,
        false,
    )
    .await
    .expect("docker run");

    assert_eq!(code, 42, "logs:\n{logs}");
}

#[tokio::test]
#[ignore = "requires Docker; set BUZ_DOCKER_E2E=1"]
async fn docker_e2e_rm_does_not_grow_container_list() {
    require_e2e();
    assert!(docker_available().await);

    let args = build_docker_run_args(
        "alpine:3.20",
        "/tmp",
        "true",
        &HashMap::new(),
        &[],
        None,
        false,
    );
    assert!(args.iter().any(|a| a == "--rm"));

    let dir = tempdir().unwrap();
    let before = Command::new("docker")
        .args(["ps", "-aq"])
        .output()
        .await
        .unwrap();
    let before_n = String::from_utf8_lossy(&before.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .count();

    let (code, logs) = run_and_collect(
        "alpine:3.20",
        dir.path(),
        "echo done",
        &HashMap::new(),
        None,
        false,
    )
    .await
    .expect("docker run");
    assert_eq!(code, 0, "logs:\n{logs}");

    tokio::time::sleep(Duration::from_millis(400)).await;
    let after = Command::new("docker")
        .args(["ps", "-aq"])
        .output()
        .await
        .unwrap();
    let after_n = String::from_utf8_lossy(&after.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .count();
    assert!(
        after_n <= before_n,
        "container count grew after --rm step (before={before_n} after={after_n})"
    );
}

#[tokio::test]
#[ignore = "requires Docker; set BUZ_DOCKER_E2E=1"]
async fn docker_e2e_kill_stops_container() {
    require_e2e();
    assert!(docker_available().await);

    let dir = tempdir().unwrap();
    let (mut child, mut rx, _guard) = execute_streaming(
        "alpine:3.20",
        dir.path(),
        "echo started; sleep 120",
        &HashMap::new(),
        &[],
        None,
        false,
        None,
    )
    .await
    .expect("spawn");

    let saw = timeout(Duration::from_secs(60), async {
        let mut buf = String::new();
        while let Some(chunk) = rx.recv().await {
            buf.push_str(&chunk);
            if buf.contains("started") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(saw, "container did not start in time");

    child.kill().await.expect("kill docker run");
    let status = timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("wait after kill timed out")
        .expect("wait");
    assert!(!status.success(), "killed process should not succeed");
}
