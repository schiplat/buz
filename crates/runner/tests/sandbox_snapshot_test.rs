//! Runner-side sandbox snapshot observer: status polling, hash-gated snapshot
//! writes, heartbeat block contents, and unreachable-supervisor reporting.

use runner::client::sandbox_snapshot::Observer;
use runner::config::Config;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// A fake sandboxd answering `status` and `get_config` from the given state.
async fn handle(stream: UnixStream, state: Value) -> anyhow::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let request: Value = serde_json::from_str(&line)?;
    let op = request["op"].as_str().unwrap_or_default();
    let response = match op {
        "status" => state["status"].clone(),
        "get_config" => state["config"].clone(),
        _ => json!({"ok": false, "error": "unsupported"}),
    };
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
    write_half.write_all(&bytes).await?;
    Ok(())
}

fn test_config(socket: &std::path::Path, snapshot: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.sandbox.supervisor_socket = socket.to_string_lossy().into_owned();
    config.sandbox.snapshot_path = snapshot.to_string_lossy().into_owned();
    config.execution_mode = shared::protocol::RunnerExecutionMode::Container;
    config
}

#[tokio::test]
async fn observer_reports_unreachable_supervisor_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("missing.sock");
    let snapshot = dir.path().join("snapshot.json");
    let config = test_config(&socket, &snapshot);
    let mut observer = Observer::default();
    let observation = observer.observe(&config).await;
    let block = &observation.block;
    assert_eq!(block["reachable"], false);
    assert!(
        !block["last_error"].as_str().unwrap_or_default().is_empty(),
        "error surfaced: {block}"
    );
    assert_eq!(block["consecutive_failures"], 1);
    assert_eq!(block["socket"], json!(socket.to_string_lossy()));
    // Second failure increments the counter.
    let observation = observer.observe(&config).await;
    assert_eq!(observation.block["consecutive_failures"], 2);
    assert!(observation.block["snapshot"]["sha256"].is_null());
    assert!(!snapshot.exists(), "no snapshot without a supervisor");
}

/// Full two-round scenario with one listener serving both ops per round.
#[tokio::test]
async fn snapshot_is_written_once_per_hash_and_skipped_when_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("sbx2.sock");
    let snapshot = dir.path().join("snapshot.json");
    let config = test_config(&socket_path, &snapshot);

    // Shared state the fake server reads per request.
    let state = std::sync::Arc::new(tokio::sync::Mutex::new(json!({
        "status": {
            "ok": true,
            "instance_id": "sandboxd-01",
            "host_id": "host-a",
            "configured": true,
            "config_version": 1,
            "config_sha256": "sha-1",
            "applied_at": "2026-10-29T00:00:00Z",
            "manager_instance_id": "mgr-1",
            "profiles": { "default": { "memory": "2g" } }
        },
        "config": {
            "ok": true, "configured": true, "version": 1, "sha256": "sha-1",
            "manager_instance_id": "mgr-1",
            "config": { "profiles": { "default": { "memory": "2g" } }, "network": "bridge" }
        }
    })));

    let listener = UnixListener::bind(&socket_path).unwrap();
    let server_state = state.clone();
    // Round 1: status + get_config, round 2: status, round 3: status + get_config.
    let server = tokio::spawn(async move {
        for _ in 0..5 {
            let (stream, _) = listener.accept().await.unwrap();
            let state = server_state.lock().await.clone();
            handle(stream, state).await.unwrap();
        }
    });

    let mut observer = Observer::default();

    // Round 1: new hash → status + get_config + snapshot write.
    let observation = observer.observe(&config).await;
    assert_eq!(
        observation.config_changed, false,
        "first round is not a change"
    );
    assert_eq!(observation.block["reachable"], true);
    assert_eq!(observation.block["config_version"], 1);
    assert_eq!(observation.block["config_sha256"], "sha-1");
    assert_eq!(observation.block["manager_instance_id"], "mgr-1");
    assert_eq!(observation.block["snapshot"]["sha256"], "sha-1");
    assert!(observation.block["profiles"]["default"].is_object());
    assert!(snapshot.exists(), "snapshot written on first observation");
    let contents: Value =
        serde_json::from_str(&std::fs::read_to_string(&snapshot).unwrap()).unwrap();
    assert_eq!(contents["version"], 1);
    assert_eq!(contents["host_id"], "host-a");
    assert_eq!(contents["sandboxd_instance_id"], "sandboxd-01");
    assert_eq!(contents["config"]["profiles"]["default"]["memory"], "2g");

    // Round 2: same hash → status only, no get_config, no rewrite.
    let before = std::fs::metadata(&snapshot).unwrap().modified().unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let observation = observer.observe(&config).await;
    assert_eq!(observation.block["config_version"], 1);
    let after = std::fs::metadata(&snapshot).unwrap().modified().unwrap();
    assert_eq!(
        before, after,
        "unchanged hash must not rewrite the snapshot"
    );

    // Round 3: ops ships v2 → config_changed fires and the snapshot updates.
    {
        let mut state = state.lock().await;
        state["status"]["config_version"] = json!(2);
        state["status"]["config_sha256"] = json!("sha-2");
        state["config"]["version"] = json!(2);
        state["config"]["sha256"] = json!("sha-2");
        state["config"]["config"]["network"] = json!("none");
    }
    let observation = observer.observe(&config).await;
    assert!(
        observation.config_changed,
        "version change must be reported"
    );
    assert_eq!(observation.block["config_version"], 2);
    assert_eq!(observation.block["snapshot"]["sha256"], "sha-2");
    let contents: Value =
        serde_json::from_str(&std::fs::read_to_string(&snapshot).unwrap()).unwrap();
    assert_eq!(contents["version"], 2);
    assert_eq!(contents["config"]["network"], "none");

    server.await.unwrap();
}
