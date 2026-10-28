use std::path::Path;

use runner::sandbox::backend::{record_probe, validate_prerequisites, TierBackends};
use shared::protocol::{ExecutionBackendStatus, RunnerExecutionCapabilities, RunnerExecutionMode};

fn backends() -> TierBackends {
    TierBackends::from_tiers([
        (RunnerExecutionMode::Container, "reg/img@sha256:a", ""),
        (RunnerExecutionMode::Sandbox, "reg/img@sha256:b", "runsc"),
        (RunnerExecutionMode::Vm, "reg/img@sha256:c", "kata-fc"),
    ])
}

/// A file standing in for `/dev/kvm`, so prerequisite checks run on any host.
fn fake_kvm() -> tempfile::NamedTempFile {
    tempfile::NamedTempFile::new().expect("temp kvm stand-in")
}

#[test]
fn configured_tiers_yield_ladder_order_and_skip_unconfigured() {
    let tiers: Vec<RunnerExecutionMode> = backends().configured().map(|d| d.mode).collect();
    assert_eq!(
        tiers,
        vec![
            RunnerExecutionMode::Container,
            RunnerExecutionMode::Sandbox,
            RunnerExecutionMode::Vm,
        ]
    );

    let partial = TierBackends::from_tiers([
        (RunnerExecutionMode::Container, "reg/img@sha256:a", ""),
        (RunnerExecutionMode::Sandbox, "", ""),
        (RunnerExecutionMode::Vm, "reg/img@sha256:c", "kata-fc"),
    ]);
    let tiers: Vec<RunnerExecutionMode> = partial.configured().map(|d| d.mode).collect();
    assert_eq!(
        tiers,
        vec![RunnerExecutionMode::Container, RunnerExecutionMode::Vm]
    );
}

#[test]
fn host_tier_has_no_descriptor() {
    assert!(backends().get(RunnerExecutionMode::Host).is_none());
}

#[test]
fn blank_runtime_means_daemon_default() {
    let table = backends();
    assert!(table
        .get(RunnerExecutionMode::Container)
        .unwrap()
        .runtime()
        .is_none());
    assert_eq!(
        table.get(RunnerExecutionMode::Sandbox).unwrap().runtime(),
        Some("runsc")
    );
}

#[test]
fn runtime_must_match_supervisor_policy_exactly() {
    let table = backends();
    let vm = table.get(RunnerExecutionMode::Vm).unwrap();
    let kvm = fake_kvm();
    assert!(validate_prerequisites(vm, "linux", kvm.path(), "kata-fc").is_ok());
    for wrong in ["runsc", "", "  "] {
        let error = validate_prerequisites(vm, "linux", kvm.path(), wrong)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("does not match supervisor policy"),
            "{error}"
        );
    }

    let container = table.get(RunnerExecutionMode::Container).unwrap();
    let error = validate_prerequisites(container, "linux", kvm.path(), "runsc")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("does not match supervisor policy"),
        "a runner cannot request a runtime the supervisor did not configure: {error}"
    );
}

#[test]
fn container_tier_needs_neither_linux_nor_kvm() {
    let table = backends();
    let container = table.get(RunnerExecutionMode::Container).unwrap();
    assert!(validate_prerequisites(container, "macos", Path::new("/nonexistent"), "").is_ok());
}

#[test]
fn sandbox_tier_requires_linux_but_not_kvm() {
    let table = backends();
    let sandbox = table.get(RunnerExecutionMode::Sandbox).unwrap();
    let error = validate_prerequisites(sandbox, "darwin", Path::new("/nonexistent"), "runsc")
        .unwrap_err()
        .to_string();
    assert!(error.contains("requires a Linux host"), "{error}");
    assert!(
        validate_prerequisites(sandbox, "linux", Path::new("/nonexistent"), "runsc").is_ok(),
        "runsc needs no /dev/kvm"
    );
}

#[test]
fn vm_tier_fails_closed_without_linux_or_kvm() {
    let table = backends();
    let vm = table.get(RunnerExecutionMode::Vm).unwrap();
    let error = validate_prerequisites(vm, "macos", Path::new("/nonexistent"), "kata-fc")
        .unwrap_err()
        .to_string();
    assert!(error.contains("requires a Linux host"), "{error}");
    let error = validate_prerequisites(vm, "linux", Path::new("/nonexistent"), "kata-fc")
        .unwrap_err()
        .to_string();
    assert!(error.contains("/dev/kvm"), "{error}");
}

#[test]
fn record_probe_fills_status_reason_and_runtime_map() {
    let table = backends();
    let mut capabilities = RunnerExecutionCapabilities::default();
    record_probe(
        &mut capabilities,
        table.get(RunnerExecutionMode::Sandbox).unwrap(),
        Ok(()),
    );
    record_probe(
        &mut capabilities,
        table.get(RunnerExecutionMode::Vm).unwrap(),
        Err(anyhow::anyhow!("no kvm")),
    );
    record_probe(
        &mut capabilities,
        table.get(RunnerExecutionMode::Container).unwrap(),
        Ok(()),
    );

    assert_eq!(capabilities.sandbox, ExecutionBackendStatus::Supported);
    assert_eq!(capabilities.vm, ExecutionBackendStatus::Unsupported);
    assert_eq!(capabilities.vm_reason.as_deref(), Some("no kvm"));
    assert_eq!(capabilities.container, ExecutionBackendStatus::Supported);
    assert_eq!(
        capabilities.runtimes.get("sandbox").map(String::as_str),
        Some("runsc")
    );
    assert_eq!(
        capabilities.runtimes.get("vm").map(String::as_str),
        Some("kata-fc"),
        "runtime is reported even when the probe fails, so operators see what was tried"
    );
    assert!(
        !capabilities.runtimes.contains_key("container"),
        "daemon-default runtime is not reported"
    );
}
