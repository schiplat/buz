use runner::sandbox::{
    cleanup_stale_containers, create_job, destroy_job, exec_step_stream, probe_backend,
    probe_capabilities, read_file, safe_work_dir, HostFacts, SandboxdConfig, Service,
};
use serde_json::json;
use shared::protocol::ExecutionBackendStatus;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

fn fake_service(root: &std::path::Path) -> (Service, tempfile::TempDir) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("docker-args");
    let docker = dir.path().join("docker");
    std::fs::write(
        &docker,
        format!(
            "#!/bin/sh\nprintf '%s' \"$*\" >> '{}'\nprintf '\\n' >> '{}'\nif [ \"$1\" = exec ]; then\n  case \"$*\" in *'sleep 30'*) sleep 30;; *) printf 'sandbox stdout'; printf 'sandbox stderr' >&2;; esac\nfi\n",
            log.display(),
            log.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o700)).unwrap();
    // A file standing in for /dev/kvm so VM-tier jobs and probes are testable
    // on hosts without hardware virtualization.
    let kvm = dir.path().join("kvm");
    std::fs::write(&kvm, "").unwrap();
    let config = SandboxdConfig {
        instance_id: "test-sandboxd".into(),
        socket: dir
            .path()
            .join("sandbox.sock")
            .to_string_lossy()
            .into_owned(),
        workspace_root: std::fs::canonicalize(root)
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        docker_cli: docker.to_string_lossy().into_owned(),
        container_image: "buz-job@sha256:abc".into(),
        container_runtime: String::new(),
        sandbox_image: "buz-sandbox@sha256:789".into(),
        sandbox_runtime: "runsc".into(),
        firecracker_guest_image: "buz-vm@sha256:def".into(),
        firecracker_runtime: "io.containerd.firecracker.v2".into(),
        socket_gid: 10001,
        workspace_volume: None,
        allowed_runner_uid: None,
        container_network: "bridge".into(),
        memory_limit: "2g".into(),
        cpu_limit: "2".into(),
    };
    let service = Service {
        config: Arc::new(config),
        jobs: Arc::new(Mutex::new(HashMap::new())),
        host: HostFacts {
            os: "linux".into(),
            kvm_path: kvm,
        },
    };
    (service, dir)
}

async fn exec_via_socket(
    service: Service,
    temp: &tempfile::TempDir,
    request: serde_json::Value,
) -> Vec<serde_json::Value> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let socket = temp.path().join("exec.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let service_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = tokio::io::BufReader::new(&mut stream);
        let mut request_line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut request_line)
            .await
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&request_line).unwrap();
        drop(reader);
        exec_step_stream(&service, &request, stream).await.unwrap();
    });
    let mut client = UnixStream::connect(&socket).await.unwrap();
    let mut request = serde_json::to_vec(&request).unwrap();
    request.push(b'\n');
    client.write_all(&request).await.unwrap();
    let mut reader = tokio::io::BufReader::new(client);
    let mut events: Vec<serde_json::Value> = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap() == 0 {
            break;
        }
        events.push(serde_json::from_str(&line).unwrap());
        if events.last().unwrap()["event"] == "exit" {
            break;
        }
    }
    service_task.await.unwrap();
    events
}

#[tokio::test]
async fn container_supervisor_create_exec_read_and_destroy() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("job-1");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join(".buz-env"), "VALUE=hello\n").unwrap();
    let (service, helper_dir) = fake_service(root.path());
    let create = create_job(
        &service,
        &json!({
            "mode":"container",
            "workspace":workspace,
            "image":"buz-job@sha256:abc"
        }),
        10001,
        10001,
    )
    .await
    .unwrap();
    let handle = create["handle"].as_str().unwrap().to_string();
    let events = exec_via_socket(
        service.clone(),
        &helper_dir,
        json!({
            "op":"exec_step",
            "handle":handle,
            "command":"echo ok",
            "work_dir":"",
            "env":{"KEY":"value"},
            "timeout_seconds":10
        }),
    )
    .await;
    assert!(events.iter().any(|event| event["event"] == "output"
        && event["stream"] == "stdout"
        && event["content"] == "sandbox stdout"));
    assert!(events.iter().any(|event| event["event"] == "output"
        && event["stream"] == "stderr"
        && event["content"] == "sandbox stderr"));
    assert!(events
        .iter()
        .any(|event| event["event"] == "exit" && event["code"] == 0));
    let file = read_file(&service, &json!({"handle":handle,"path":".buz-env"}))
        .await
        .unwrap();
    assert_eq!(file["content"], "VALUE=hello\n");

    let log = std::fs::read_to_string(helper_dir.path().join("docker-args")).unwrap();
    assert!(
        log.contains("--read-only"),
        "container hardening flags: {log}"
    );
    assert!(
        log.contains("--cap-drop ALL"),
        "container capabilities: {log}"
    );
    assert!(log.contains("--user 10001:10001"), "peer identity: {log}");
    assert!(
        log.contains("timeout -s TERM -k 5 10s"),
        "step timeout must use busybox-compatible short flags: {log}"
    );
    assert!(
        !log.contains("--signal"),
        "GNU-only long flags break busybox-based images: {log}"
    );
    assert!(
        !log.contains("--runtime"),
        "container tier defaults to the daemon runtime: {log}"
    );
    destroy_job(&service, &json!({"handle":handle}))
        .await
        .unwrap();
    assert!(service.jobs.lock().await.is_empty());
}

#[tokio::test]
async fn cancelling_exec_connection_removes_the_job_container() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("job-cancel");
    std::fs::create_dir(&workspace).unwrap();
    let (service, helper_dir) = fake_service(root.path());
    let created = create_job(
        &service,
        &json!({"mode":"container","workspace":workspace,"image":"buz-job@sha256:abc"}),
        10001,
        10001,
    )
    .await
    .unwrap();
    let handle = created["handle"].as_str().unwrap().to_string();
    let socket = helper_dir.path().join("cancel.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let service_copy = service.clone();
    let request =
        json!({"handle":handle,"command":"sleep 30","work_dir":"","env":{},"timeout_seconds":0});
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        exec_step_stream(&service_copy, &request, stream)
            .await
            .unwrap();
    });
    let mut client = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let mut payload = serde_json::to_vec(&json!({"op":"exec_step","handle":handle,"command":"sleep 30","work_dir":"","env":{},"timeout_seconds":0})).unwrap();
    payload.push(b'\n');
    use tokio::io::AsyncWriteExt;
    client.write_all(&payload).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert!(
        std::fs::read_to_string(helper_dir.path().join("docker-args"))
            .unwrap()
            .contains("rm --force")
    );
    assert!(service.jobs.lock().await.contains_key(&handle));
}

#[tokio::test]
async fn slow_client_cannot_starve_the_step_watchdog() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("job-slow-reader");
    std::fs::create_dir(&workspace).unwrap();
    let (service, helper_dir) = fake_service(root.path());
    let created = create_job(
        &service,
        &json!({"mode":"container","workspace":workspace,"image":"buz-job@sha256:abc"}),
        10001,
        10001,
    )
    .await
    .unwrap();
    let handle = created["handle"].as_str().unwrap().to_string();
    let socket = helper_dir.path().join("slow-reader.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let service_copy = service.clone();
    let request =
        json!({"handle":handle,"command":"echo ok","work_dir":"","env":{},"timeout_seconds":1});
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        exec_step_stream(&service_copy, &request, stream).await
    });
    let mut client = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let mut payload = serde_json::to_vec(&json!({"op":"exec_step","handle":handle,"command":"echo ok","work_dir":"","env":{},"timeout_seconds":1})).unwrap();
    payload.push(b'\n');
    use tokio::io::AsyncWriteExt;
    client.write_all(&payload).await.unwrap();
    // Deliberately do not read: fill the server's socket send buffer.
    let outcome = tokio::time::timeout(Duration::from_secs(30), server)
        .await
        .expect("watchdog must finish despite unread output");
    // Either the in-container timeout produced a normal exit frame flow or the
    // write deadline fired; both paths must terminate and clean the container.
    let _ = outcome;
    assert!(
        std::fs::read_to_string(helper_dir.path().join("docker-args"))
            .unwrap()
            .contains("rm --force"),
        "slow-reader watchdog must still clean the container"
    );
}

#[tokio::test]
async fn vm_mode_uses_firecracker_compatible_runtime() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("job-vm");
    std::fs::create_dir(&workspace).unwrap();
    let (service, helper_dir) = fake_service(root.path());
    let created = create_job(
        &service,
        &json!({
            "mode":"vm",
            "workspace":workspace,
            "image":"buz-vm@sha256:def",
            "oci_runtime":"io.containerd.firecracker.v2"
        }),
        10001,
        10001,
    )
    .await
    .unwrap();
    let log = std::fs::read_to_string(helper_dir.path().join("docker-args")).unwrap();
    assert!(
        log.contains("--runtime io.containerd.firecracker.v2"),
        "VM runtime: {log}"
    );
    destroy_job(&service, &json!({"handle":created["handle"]}))
        .await
        .unwrap();
}

#[tokio::test]
async fn sandbox_tier_uses_the_gvisor_runtime_without_kvm_requirements() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("job-sandbox");
    std::fs::create_dir(&workspace).unwrap();
    let (service, helper_dir) = fake_service(root.path());
    // Host without /dev/kvm: the sandbox tier must still be selectable.
    let service = Service {
        config: service.config.clone(),
        jobs: service.jobs.clone(),
        host: HostFacts {
            os: "linux".into(),
            kvm_path: root.path().join("no-kvm-here"),
        },
    };
    let created = create_job(
        &service,
        &json!({
            "mode":"sandbox",
            "workspace":workspace,
            "image":"buz-sandbox@sha256:789",
            "oci_runtime":"runsc"
        }),
        10001,
        10001,
    )
    .await
    .unwrap();
    let log = std::fs::read_to_string(helper_dir.path().join("docker-args")).unwrap();
    assert!(log.contains("--runtime runsc"), "sandbox runtime: {log}");
    destroy_job(&service, &json!({"handle":created["handle"]}))
        .await
        .unwrap();
}

#[tokio::test]
async fn job_start_fails_closed_when_host_drifted_since_the_probe() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("job-drift");
    std::fs::create_dir(&workspace).unwrap();
    let (service, _helper_dir) = fake_service(root.path());
    // The probe saw /dev/kvm, but the host lost it before the job started.
    let mut config = (*service.config).clone();
    config.workspace_volume = None;
    let drifted = Service {
        config: Arc::new(config),
        jobs: service.jobs.clone(),
        host: HostFacts {
            os: "linux".into(),
            kvm_path: root.path().join("kvm-gone"),
        },
    };
    let outcome = create_job(
        &drifted,
        &json!({
            "mode":"vm",
            "workspace":workspace,
            "image":"buz-vm@sha256:def",
            "oci_runtime":"io.containerd.firecracker.v2"
        }),
        10001,
        10001,
    )
    .await;
    let error = outcome.unwrap_err().to_string();
    assert!(error.contains("/dev/kvm"), "drift re-check: {error}");

    // A runtime mismatch against supervisor policy also fails closed.
    let mismatch = create_job(
        &drifted,
        &json!({
            "mode":"sandbox",
            "workspace":root.path().join("job-drift"),
            "image":"buz-sandbox@sha256:789",
            "oci_runtime":"kata-fc"
        }),
        10001,
        10001,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        mismatch.contains("does not match supervisor policy"),
        "runtime mismatch: {mismatch}"
    );
}

#[tokio::test]
async fn unsupported_mode_and_foreign_image_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("job-3");
    std::fs::create_dir(&workspace).unwrap();
    let (service, _helper_dir) = fake_service(root.path());
    let host_mode = create_job(
        &service,
        &json!({"mode":"host","workspace":workspace,"image":"buz-job@sha256:abc"}),
        10001,
        10001,
    )
    .await;
    let error = host_mode.unwrap_err().to_string();
    assert!(error.contains("unsupported sandbox mode"), "{error}");
    let unknown_mode = create_job(
        &service,
        &json!({"mode":"gvisor","workspace":workspace,"image":"buz-job@sha256:abc"}),
        10001,
        10001,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        unknown_mode.contains("unsupported sandbox mode"),
        "{unknown_mode}"
    );
}

#[tokio::test]
async fn supervisor_rejects_workspace_outside_configured_root_and_wrong_image() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let (service, _helper_dir) = fake_service(root.path());
    let outside_attempt = create_job(
        &service,
        &json!({
            "mode":"container",
            "workspace":outside.path(),
            "image":"buz-job@sha256:abc"
        }),
        10001,
        10001,
    )
    .await;
    assert!(outside_attempt.is_err());

    let inside = root.path().join("job-2");
    std::fs::create_dir(&inside).unwrap();
    let wrong_image = create_job(
        &service,
        &json!({"mode":"container","workspace":inside,"image":"evil:latest"}),
        10001,
        10001,
    )
    .await;
    assert!(wrong_image.is_err());
}

#[test]
fn supervisor_work_dir_rejects_absolute_and_parent_paths() {
    assert!(safe_work_dir("").unwrap().is_empty());
    assert_eq!(safe_work_dir("src/project").unwrap(), "src/project");
    assert!(safe_work_dir("/etc").is_err());
    assert!(safe_work_dir("../outside").is_err());
}

#[tokio::test]
async fn sandboxd_mounts_only_job_volume_subpath_when_configured() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("job-boundary");
    std::fs::create_dir(&workspace).unwrap();
    let (mut service, helper_dir) = fake_service(root.path());
    let mut config = (*service.config).clone();
    config.workspace_volume = Some("buz-runner-ws".into());
    service.config = Arc::new(config);

    let created = create_job(
        &service,
        &json!({
            "mode":"container",
            "workspace":workspace,
            "image":"buz-job@sha256:abc"
        }),
        10001,
        10001,
    )
    .await
    .unwrap();
    let args = std::fs::read_to_string(helper_dir.path().join("docker-args")).unwrap();
    assert!(
        args.contains(
            "type=volume,source=buz-runner-ws,dst=/buz/workspace,volume-subpath=job-boundary"
        ),
        "job-only volume mount: {args}"
    );
    destroy_job(&service, &json!({"handle":created["handle"]}))
        .await
        .unwrap();
}

#[tokio::test]
async fn capability_probe_reports_every_tier_with_runtimes_and_reasons() {
    let root = tempfile::tempdir().unwrap();
    let (service, _helper_dir) = fake_service(root.path());
    let response = probe_capabilities(
        &service,
        &json!({"op":"probe_capabilities","backends":[
            {"mode":"container","image":"buz-job@sha256:abc","oci_runtime":""},
            {"mode":"sandbox","image":"buz-sandbox@sha256:789","oci_runtime":"runsc"},
            {"mode":"vm","image":"buz-vm@sha256:def","oci_runtime":"io.containerd.firecracker.v2"}
        ]}),
    )
    .await
    .unwrap();
    let capabilities: shared::protocol::RunnerExecutionCapabilities =
        serde_json::from_value(response["capabilities"].clone()).unwrap();
    assert_eq!(capabilities.container, ExecutionBackendStatus::Supported);
    assert_eq!(capabilities.sandbox, ExecutionBackendStatus::Supported);
    assert_eq!(capabilities.vm, ExecutionBackendStatus::Supported);
    assert!(capabilities.container_reason.is_none());
    assert_eq!(
        capabilities.runtimes.get("sandbox").map(String::as_str),
        Some("runsc")
    );
    assert_eq!(
        capabilities.runtimes.get("vm").map(String::as_str),
        Some("io.containerd.firecracker.v2")
    );
    assert!(!capabilities.runtimes.contains_key("container"));
}

#[tokio::test]
async fn capability_probe_fails_closed_for_unconfigured_and_legacy_clients() {
    let root = tempfile::tempdir().unwrap();
    let (service, _helper_dir) = fake_service(root.path());
    let mut config = (*service.config).clone();
    config.sandbox_image = String::new();
    config.sandbox_runtime = String::new();
    let service = Service {
        config: Arc::new(config),
        jobs: service.jobs.clone(),
        host: service.host.clone(),
    };

    // A tier the supervisor does not know gets a reason, never silence.
    let response = probe_capabilities(
        &service,
        &json!({"op":"probe_capabilities","backends":[
            {"mode":"container","image":"buz-job@sha256:abc","oci_runtime":""},
            {"mode":"sandbox","image":"x@sha256:1","oci_runtime":"runsc"}
        ]}),
    )
    .await
    .unwrap();
    let capabilities: shared::protocol::RunnerExecutionCapabilities =
        serde_json::from_value(response["capabilities"].clone()).unwrap();
    assert_eq!(capabilities.container, ExecutionBackendStatus::Supported);
    assert_eq!(capabilities.sandbox, ExecutionBackendStatus::Unsupported);
    assert!(
        capabilities
            .sandbox_reason
            .as_deref()
            .unwrap_or_default()
            .contains("not configured"),
        "unconfigured tier reason: {:?}",
        capabilities.sandbox_reason
    );
    assert_eq!(capabilities.vm, ExecutionBackendStatus::Unsupported);
    assert!(
        capabilities
            .vm_reason
            .as_deref()
            .unwrap_or_default()
            .contains("does not match supervisor policy"),
        "vm tier was claimed by no runner-side backend entry"
    );

    // Legacy runner payload (no `backends` key): everything fails closed.
    let response = probe_capabilities(&service, &json!({"op":"probe_capabilities"}))
        .await
        .unwrap();
    let capabilities: shared::protocol::RunnerExecutionCapabilities =
        serde_json::from_value(response["capabilities"].clone()).unwrap();
    assert_eq!(capabilities.container, ExecutionBackendStatus::Unsupported);
    assert!(
        capabilities
            .container_reason
            .as_deref()
            .unwrap_or_default()
            .contains("does not match"),
        "legacy payload cannot claim the pinned image: {:?}",
        capabilities.container_reason
    );
}

#[tokio::test]
async fn capability_probe_removes_probe_container_when_exec_fails() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("docker-fail.log");
    let script = dir.path().join("docker-fail");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1\" in create) printf 'probe-container\\n';; start) printf 'probe-container\\n';; exec) exit 1;; rm) exit 0;; esac\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let image = "job@sha256:abc".to_string();
    let kvm = dir.path().join("kvm");
    std::fs::write(&kvm, "").unwrap();
    let config = SandboxdConfig {
        instance_id: "test-sandboxd".into(),
        socket: dir.path().join("sock").display().to_string(),
        workspace_root: dir.path().display().to_string(),
        docker_cli: script.display().to_string(),
        container_image: image.clone(),
        container_runtime: String::new(),
        sandbox_image: String::new(),
        sandbox_runtime: String::new(),
        firecracker_guest_image: image.clone(),
        firecracker_runtime: "firecracker".into(),
        socket_gid: 10001,
        workspace_volume: None,
        allowed_runner_uid: Some(10001),
        container_network: "none".into(),
        memory_limit: "1g".into(),
        cpu_limit: "1".into(),
    };
    let service = Service {
        config: Arc::new(config),
        jobs: Arc::new(Mutex::new(HashMap::new())),
        host: HostFacts {
            os: "linux".into(),
            kvm_path: kvm,
        },
    };
    let backends = service.config.backends();
    let descriptor = backends
        .get(shared::protocol::RunnerExecutionMode::Container)
        .unwrap();
    assert!(probe_backend(&service, descriptor, &image).await.is_err());
    assert!(std::fs::read_to_string(log)
        .unwrap()
        .contains("rm --force buz-sandbox-probe-"));
}

#[tokio::test]
async fn stale_cleanup_only_removes_this_instance_containers() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("docker-recovery.log");
    let script = dir.path().join("docker-recovery");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = ps ] && printf '%s\\n' \"$*\" | grep -q 'instance=instance-a'; then printf 'stale-own\\n'; fi\nif [ \"$1\" = rm ]; then exit 0; fi\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let image = "job@sha256:abc".to_string();
    let config = SandboxdConfig {
        instance_id: "instance-a".into(),
        socket: dir.path().join("sock").display().to_string(),
        workspace_root: dir.path().display().to_string(),
        docker_cli: script.display().to_string(),
        container_image: image,
        container_runtime: String::new(),
        sandbox_image: String::new(),
        sandbox_runtime: String::new(),
        firecracker_guest_image: String::new(),
        firecracker_runtime: String::new(),
        socket_gid: 10001,
        workspace_volume: None,
        allowed_runner_uid: Some(10001),
        container_network: "none".into(),
        memory_limit: "1g".into(),
        cpu_limit: "1".into(),
    };
    cleanup_stale_containers(&config).await.unwrap();
    let log = std::fs::read_to_string(log).unwrap();
    assert!(
        log.contains("label=buz.sandboxd.instance=instance-a"),
        "recovery must filter by its own instance label: {log}"
    );
    assert!(log.contains("rm --force stale-own"));
    assert!(
        !log.contains("stale-foreign"),
        "foreign-instance ids must not be listed or removed: {log}"
    );
}
