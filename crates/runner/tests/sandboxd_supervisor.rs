//! `buz-sandboxd` supervisor tests against a fake Docker CLI.
//!
//! Configuration model under test: host-local `Bootstrap` plus a
//! Manager-managed, atomically applied document (`ConfigState`). Jobs name a
//! tier and a resource profile; image, runtime, and limits come from the
//! applied document — never from the Runner.

use runner::sandbox::{
    cleanup_stale_containers, create_job, destroy_job, exec_step_stream, get_config, probe_backend,
    probe_capabilities, read_file, safe_work_dir, status, AppliedConfig, Bootstrap, ConfigState,
    HostFacts, Service,
};
use serde_json::json;
use shared::protocol::{ExecutionBackendStatus, RunnerExecutionMode};
use shared::sandbox_config::{ResourceProfile, SandboxHostDocument, TierBackend};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// The three-tier document most tests apply: container (runc), sandbox
/// (runsc), vm (firecracker-compatible), with `default` and `large` profiles.
fn full_document() -> SandboxHostDocument {
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "default".into(),
        ResourceProfile {
            memory: "2g".into(),
            cpu: "2".into(),
            pids: 256,
            slots: 1,
        },
    );
    profiles.insert(
        "large".into(),
        ResourceProfile {
            memory: "8g".into(),
            cpu: "4".into(),
            pids: 512,
            slots: 4,
        },
    );
    let mut tiers = BTreeMap::new();
    tiers.insert(
        "container".into(),
        TierBackend {
            image: "buz-job@sha256:abc".into(),
            runtime: String::new(),
        },
    );
    tiers.insert(
        "sandbox".into(),
        TierBackend {
            image: "buz-sandbox@sha256:789".into(),
            runtime: "runsc".into(),
        },
    );
    tiers.insert(
        "vm".into(),
        TierBackend {
            image: "buz-vm@sha256:def".into(),
            runtime: "io.containerd.firecracker.v2".into(),
        },
    );
    SandboxHostDocument {
        profiles,
        exclude_profiles: Default::default(),
        tiers,
        network: "bridge".into(),
    }
}

fn applied(document: SandboxHostDocument, version: i64) -> AppliedConfig {
    let (document, _) = shared::sandbox_config::validate(&document).expect("test doc valid");
    let json = serde_json::to_string(&shared::sandbox_config::canonical_json(&document)).unwrap();
    AppliedConfig {
        version,
        sha256: shared::sandbox_config::sha256_hex(json.as_bytes()),
        manager_instance_id: "mgr-test".into(),
        applied_at: "2026-10-29T00:00:00Z".into(),
        document,
    }
}

fn fake_bootstrap(
    dir: &std::path::Path,
    root: &std::path::Path,
    docker: &std::path::Path,
) -> Bootstrap {
    Bootstrap {
        instance_id: "test-sandboxd".into(),
        host_id: "host-test".into(),
        socket: dir.join("sandbox.sock").to_string_lossy().into_owned(),
        workspace_root: std::fs::canonicalize(root)
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        workspace_volume: None,
        docker_cli: docker.to_string_lossy().into_owned(),
        socket_gid: 10001,
        allowed_runner_uid: None,
        manager_url: String::new(),
        manager_instance_id: String::new(),
        token: String::new(),
        tls_skip_verify: false,
        state_dir: dir.join("state"),
    }
}

/// Service with the fake docker, a fake /dev/kvm, and the full document applied.
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
    let service = Service {
        bootstrap: Arc::new(fake_bootstrap(dir.path(), root, &docker)),
        config: ConfigState::new(Some(applied(full_document(), 7))),
        jobs: Arc::new(Mutex::new(HashMap::new())),
        host: HostFacts {
            os: "linux".into(),
            kvm_path: kvm,
        },
    };
    (service, dir)
}

fn with_bootstrap(service: &Service, edit: impl FnOnce(&mut Bootstrap)) -> Service {
    let mut bootstrap = (*service.bootstrap).clone();
    edit(&mut bootstrap);
    Service {
        bootstrap: Arc::new(bootstrap),
        config: service.config.clone(),
        jobs: service.jobs.clone(),
        host: service.host.clone(),
    }
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
        &json!({"mode":"container","workspace":workspace}),
        10001,
        10001,
    )
    .await
    .unwrap();
    let handle = create["handle"].as_str().unwrap().to_string();
    assert_eq!(create["profile"], "default");
    assert_eq!(create["config_version"], 7);
    assert_eq!(create["limits"]["memory"], "2g");
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
    // Default envelope from the applied document, including the formerly
    // hard-coded pids limit.
    assert!(
        log.contains("--memory 2g --cpus 2"),
        "default envelope: {log}"
    );
    assert!(log.contains("--pids-limit 256"), "pids from profile: {log}");
    assert!(
        log.contains("--network bridge"),
        "network from document: {log}"
    );
    assert!(
        log.contains("buz-job@sha256:abc"),
        "image from document: {log}"
    );
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
async fn named_profile_sets_the_container_envelope() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("job-large");
    std::fs::create_dir(&workspace).unwrap();
    let (service, helper_dir) = fake_service(root.path());
    let create = create_job(
        &service,
        &json!({"mode":"container","workspace":workspace,"profile":"large"}),
        10001,
        10001,
    )
    .await
    .unwrap();
    assert_eq!(create["profile"], "large");
    assert_eq!(create["limits"]["slots"], 4);
    let log = std::fs::read_to_string(helper_dir.path().join("docker-args")).unwrap();
    assert!(
        log.contains("--memory 8g --cpus 4"),
        "large envelope: {log}"
    );
    assert!(log.contains("--pids-limit 512"), "large pids: {log}");
    destroy_job(&service, &json!({"handle":create["handle"]}))
        .await
        .unwrap();
}

#[tokio::test]
async fn unknown_profile_is_rejected_not_downgraded() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("job-xl");
    std::fs::create_dir(&workspace).unwrap();
    let (service, helper_dir) = fake_service(root.path());
    let error = create_job(
        &service,
        &json!({"mode":"container","workspace":workspace,"profile":"xlarge"}),
        10001,
        10001,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("'xlarge'"), "unknown profile: {error}");
    assert!(
        !helper_dir.path().join("docker-args").exists(),
        "no container may be created for an unknown profile"
    );
}

#[tokio::test]
async fn no_applied_configuration_fails_closed() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("job-noconf");
    std::fs::create_dir(&workspace).unwrap();
    let (service, helper_dir) = fake_service(root.path());
    let unconfigured = Service {
        bootstrap: service.bootstrap.clone(),
        config: ConfigState::new(None),
        jobs: service.jobs.clone(),
        host: service.host.clone(),
    };
    let error = create_job(
        &unconfigured,
        &json!({"mode":"container","workspace":workspace}),
        10001,
        10001,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("no applied configuration"), "{error}");
    assert!(!helper_dir.path().join("docker-args").exists());

    // Probes still answer — every tier unsupported with the reason.
    let response = probe_capabilities(&unconfigured).await.unwrap();
    let capabilities: shared::protocol::RunnerExecutionCapabilities =
        serde_json::from_value(response["capabilities"].clone()).unwrap();
    assert_eq!(capabilities.container, ExecutionBackendStatus::Unsupported);
    assert!(capabilities
        .container_reason
        .as_deref()
        .unwrap_or_default()
        .contains("no applied configuration"));
    assert_eq!(response["config_version"], 0);

    // And status reports the state honestly.
    let status = status(&unconfigured);
    assert_eq!(status["configured"], false);
    assert_eq!(status["config_version"], 0);
    assert_eq!(get_config(&unconfigured)["configured"], false);
}

#[tokio::test]
async fn config_swap_applies_to_new_jobs_only() {
    let root = tempfile::tempdir().unwrap();
    let ws1 = root.path().join("job-before");
    let ws2 = root.path().join("job-after");
    std::fs::create_dir(&ws1).unwrap();
    std::fs::create_dir(&ws2).unwrap();
    let (service, helper_dir) = fake_service(root.path());

    let first = create_job(
        &service,
        &json!({"mode":"container","workspace":ws1}),
        10001,
        10001,
    )
    .await
    .unwrap();
    assert_eq!(first["config_version"], 7);

    // Ops shrinks the default envelope and ships v8.
    let mut doc = full_document();
    doc.profiles.get_mut("default").unwrap().memory = "1g".into();
    service.config.replace(applied(doc, 8));

    let second = create_job(
        &service,
        &json!({"mode":"container","workspace":ws2}),
        10001,
        10001,
    )
    .await
    .unwrap();
    assert_eq!(second["config_version"], 8);
    assert_eq!(second["limits"]["memory"], "1g");

    let log = std::fs::read_to_string(helper_dir.path().join("docker-args")).unwrap();
    assert!(
        log.contains("--memory 2g"),
        "first job kept its envelope: {log}"
    );
    assert!(
        log.contains("--memory 1g"),
        "second job used the new envelope: {log}"
    );
    assert_eq!(status(&service)["config_version"], 8);
    assert_eq!(get_config(&service)["version"], 8);
    assert_eq!(
        get_config(&service)["config"]["profiles"]["default"]["memory"],
        "1g"
    );
}

#[tokio::test]
async fn cancelling_exec_connection_removes_the_job_container() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("job-cancel");
    std::fs::create_dir(&workspace).unwrap();
    let (service, helper_dir) = fake_service(root.path());
    let created = create_job(
        &service,
        &json!({"mode":"container","workspace":workspace}),
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
        &json!({"mode":"container","workspace":workspace}),
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
        &json!({"mode":"vm","workspace":workspace}),
        10001,
        10001,
    )
    .await
    .unwrap();
    let log = std::fs::read_to_string(helper_dir.path().join("docker-args")).unwrap();
    assert!(
        log.contains("--runtime io.containerd.firecracker.v2"),
        "VM runtime from document: {log}"
    );
    assert!(
        log.contains("buz-vm@sha256:def"),
        "VM image from document: {log}"
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
        bootstrap: service.bootstrap.clone(),
        config: service.config.clone(),
        jobs: service.jobs.clone(),
        host: HostFacts {
            os: "linux".into(),
            kvm_path: root.path().join("no-kvm-here"),
        },
    };
    let created = create_job(
        &service,
        &json!({"mode":"sandbox","workspace":workspace}),
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
    let drifted = Service {
        bootstrap: service.bootstrap.clone(),
        config: service.config.clone(),
        jobs: service.jobs.clone(),
        host: HostFacts {
            os: "linux".into(),
            kvm_path: root.path().join("kvm-gone"),
        },
    };
    let error = create_job(
        &drifted,
        &json!({"mode":"vm","workspace":workspace}),
        10001,
        10001,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("/dev/kvm"), "drift re-check: {error}");
}

#[tokio::test]
async fn unsupported_mode_and_unconfigured_tier_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("job-3");
    std::fs::create_dir(&workspace).unwrap();
    let (service, _helper_dir) = fake_service(root.path());
    let host_mode = create_job(
        &service,
        &json!({"mode":"host","workspace":workspace}),
        10001,
        10001,
    )
    .await;
    let error = host_mode.unwrap_err().to_string();
    assert!(error.contains("unsupported sandbox mode"), "{error}");
    let unknown_mode = create_job(
        &service,
        &json!({"mode":"gvisor","workspace":workspace}),
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

    // A document that omits a tier makes that tier unavailable.
    let mut doc = full_document();
    doc.tiers.remove("sandbox");
    service.config.replace(applied(doc, 9));
    let missing_tier = create_job(
        &service,
        &json!({"mode":"sandbox","workspace":workspace}),
        10001,
        10001,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        missing_tier.contains("not configured on this sandbox host"),
        "{missing_tier}"
    );
}

#[tokio::test]
async fn supervisor_rejects_workspace_outside_configured_root() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let (service, _helper_dir) = fake_service(root.path());
    let outside_attempt = create_job(
        &service,
        &json!({"mode":"container","workspace":outside.path()}),
        10001,
        10001,
    )
    .await;
    assert!(outside_attempt.is_err());
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
    let (service, helper_dir) = fake_service(root.path());
    let service = with_bootstrap(&service, |b| {
        b.workspace_volume = Some("buz-runner-ws".into())
    });

    let created = create_job(
        &service,
        &json!({"mode":"container","workspace":workspace}),
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
    let response = probe_capabilities(&service).await.unwrap();
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
    assert_eq!(response["config_version"], 7);
}

#[tokio::test]
async fn capability_probe_fails_closed_for_unconfigured_tiers() {
    let root = tempfile::tempdir().unwrap();
    let (service, _helper_dir) = fake_service(root.path());
    let mut doc = full_document();
    doc.tiers.remove("sandbox");
    doc.tiers.remove("vm");
    service.config.replace(applied(doc, 3));

    let response = probe_capabilities(&service).await.unwrap();
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
    let kvm = dir.path().join("kvm");
    std::fs::write(&kvm, "").unwrap();
    let mut bootstrap = fake_bootstrap(dir.path(), dir.path(), &script);
    bootstrap.allowed_runner_uid = Some(10001);
    let service = Service {
        bootstrap: Arc::new(bootstrap),
        config: ConfigState::new(Some(applied(full_document(), 1))),
        jobs: Arc::new(Mutex::new(HashMap::new())),
        host: HostFacts {
            os: "linux".into(),
            kvm_path: kvm,
        },
    };
    let applied_config = service.config.require().unwrap();
    let backends = applied_config.backends();
    let descriptor = backends.get(RunnerExecutionMode::Container).unwrap();
    assert!(probe_backend(&service, &applied_config, descriptor)
        .await
        .is_err());
    let log = std::fs::read_to_string(log).unwrap();
    assert!(log.contains("rm --force buz-sandbox-probe-"));
    // Probes run under the default envelope.
    assert!(
        log.contains("--memory 2g --cpus 2"),
        "probe envelope: {log}"
    );
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
    let mut bootstrap = fake_bootstrap(dir.path(), dir.path(), &script);
    bootstrap.instance_id = "instance-a".into();
    bootstrap.allowed_runner_uid = Some(10001);
    cleanup_stale_containers(&bootstrap).await.unwrap();
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

#[test]
fn cache_round_trips_and_refuses_foreign_identities() {
    use runner::sandbox::manager_fetch::load_cache;
    let dir = tempfile::tempdir().unwrap();
    let docker = dir.path().join("docker");
    std::fs::write(&docker, "#!/bin/sh\n").unwrap();
    let bootstrap = fake_bootstrap(dir.path(), dir.path(), &docker);

    // No cache yet.
    assert!(load_cache(&bootstrap).unwrap().is_none());

    // Write a cache record as the fetch loop would, then load it back.
    let applied_doc = applied(full_document(), 5);
    std::fs::create_dir_all(&bootstrap.state_dir).unwrap();
    std::fs::write(
        bootstrap.cache_path(),
        serde_json::to_vec(&json!({
            "manager_instance_id": "mgr-test",
            "host_id": bootstrap.host_id,
            "instance_id": bootstrap.instance_id,
            "version": 5,
            "sha256": applied_doc.sha256,
            "applied_at": applied_doc.applied_at,
            "document": shared::sandbox_config::canonical_json(&applied_doc.document),
        }))
        .unwrap(),
    )
    .unwrap();
    let loaded = load_cache(&bootstrap).unwrap().expect("cache loads");
    assert_eq!(loaded.version, 5);
    assert_eq!(loaded.document, applied_doc.document);

    // Pinned to a different Manager → refused.
    let mut pinned = bootstrap.clone();
    pinned.manager_instance_id = "mgr-other".into();
    let error = load_cache(&pinned).unwrap_err().to_string();
    assert!(error.contains("pinned to 'mgr-other'"), "{error}");

    // A different instance id → refused.
    let mut other_instance = bootstrap.clone();
    other_instance.instance_id = "sandboxd-02".into();
    assert!(load_cache(&other_instance).is_err());
}
