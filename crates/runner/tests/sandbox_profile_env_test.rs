//! Claim payload → supervisor → step env: a job's frozen `resource_profile`
//! must reach the sandboxed step as `$BUZ_RESOURCE_PROFILE`, carrying the
//! profile the supervisor reports as effective for that job.
//!
//! Driven end to end through `execute_job` with two fakes: a Unix-socket
//! supervisor that echoes the profile it applied, and an HTTP stub standing in
//! for the Manager (only job-status uploads need a 2xx).

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use runner::config::Config;
use shared::protocol::RunnerExecutionMode;

/// Accepts any HTTP request and answers `200 {"success":true,"data":null}`.
async fn spawn_stub_manager() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let read = stream.read(&mut chunk).await.unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    let Some(head_end) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&request[..head_end]).to_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    while request.len() < head_end + 4 + length {
                        let read = stream.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..read]);
                    }
                    break;
                }
                let body = r#"{"success":true,"data":null}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://{address}")
}

/// One-request-per-connection supervisor: records every request, answers
/// `create_job` with `profile: "large"`, and streams one output + exit event
/// for `exec_step`.
fn spawn_stub_supervisor(socket: std::path::PathBuf) -> Arc<Mutex<Vec<Value>>> {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    // Bind before returning so the first connect cannot race the listener.
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let recorder = Arc::clone(&recorder);
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut line = String::new();
                if BufReader::new(read_half)
                    .read_line(&mut line)
                    .await
                    .unwrap_or(0)
                    == 0
                {
                    return;
                }
                let request: Value = match serde_json::from_str(&line) {
                    Ok(request) => request,
                    Err(_) => return,
                };
                recorder.lock().unwrap().push(request.clone());
                let reply = match request["op"].as_str() {
                    Some("create_job") => {
                        json!({"ok":true,"handle":"handle-1","profile":"large","config_version":7})
                    }
                    Some("exec_step") => {
                        let events = "{\"event\":\"output\",\"stream\":\"stdout\",\"content\":\"ran\\n\"}\n{\"event\":\"exit\",\"code\":0,\"timed_out\":false}\n";
                        let _ = write_half.write_all(events.as_bytes()).await;
                        return;
                    }
                    Some("destroy_job") => json!({"ok":true}),
                    other => json!({"ok":false,"error":format!("unexpected op {other:?}")}),
                };
                let _ = write_half.write_all(format!("{reply}\n").as_bytes()).await;
            });
        }
    });
    seen
}

#[tokio::test]
async fn sandboxed_step_exports_the_effective_resource_profile() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_root = dir.path().join("jobs");
    let workspace = workspace_root.join("job-1");
    std::fs::create_dir_all(&workspace).unwrap();
    let socket = dir.path().join("sandboxd.sock");
    let seen = spawn_stub_supervisor(socket.clone());
    let manager_url = spawn_stub_manager().await;

    let config = Config {
        execution_mode: RunnerExecutionMode::Sandbox,
        runner: runner::config::RunnerSection {
            manager_url,
            workspace_dir: workspace.to_string_lossy().into_owned(),
            ..Default::default()
        },
        sandbox: runner::config::SandboxSection {
            supervisor_socket: socket.to_string_lossy().into_owned(),
            workspace_root: workspace_root.to_string_lossy().into_owned(),
            ..Default::default()
        },
        ..Default::default()
    };
    let task = json!({
        "job_id": "job-1",
        "resource_profile": "large",
        "tasks": [{
            "name": "probe",
            "exec_type": "shell",
            "command_template": "echo hi"
        }]
    });

    let (_cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    runner::client::poll::execute_job(
        &reqwest::Client::new(),
        &config,
        "runner-token",
        &task,
        &mut cancel_rx,
    )
    .await
    .expect("sandboxed job runs against the stub supervisor");

    let requests = seen.lock().unwrap().clone();
    let create = requests
        .iter()
        .find(|request| request["op"] == "create_job")
        .expect("supervisor received create_job");
    assert_eq!(create["profile"], "large");
    assert_eq!(create["mode"], "sandbox");

    let exec = requests
        .iter()
        .find(|request| request["op"] == "exec_step")
        .expect("supervisor received exec_step");
    assert_eq!(exec["env"]["BUZ_RESOURCE_PROFILE"], "large");
    assert_eq!(exec["env"]["BUZ_STEP_ENGINE"], "shell");

    assert!(
        requests
            .iter()
            .any(|request| request["op"] == "destroy_job"),
        "the job sandbox is destroyed after the run"
    );
}
