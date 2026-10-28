use shared::protocol::{
    ExecutionBackendStatus, RunnerExecutionCapabilities, RunnerExecutionMode, SystemInfo,
};

#[test]
fn legacy_system_info_without_capabilities_remains_compatible() {
    let info: SystemInfo = serde_json::from_str(
        r#"{"runner_version":"1","hostname":"h","os":"Linux","kernel_version":"k","cpu_arch":"x86_64","cpu_cores":1,"cpu_model":"cpu","cpu_usage_percent":0.0,"memory_total_mb":1,"memory_used_mb":0,"swap_total_mb":0,"swap_used_mb":0,"disk_total_gb":1.0,"disk_used_gb":0.0,"network_interfaces":[],"ip_addresses":[],"uptime_seconds":1}"#,
    )
    .unwrap();
    assert!(info.execution_capabilities.is_none());
}

#[test]
fn execution_capabilities_round_trip_with_reasons_and_timestamp() {
    let capabilities = RunnerExecutionCapabilities {
        container: ExecutionBackendStatus::Supported,
        container_reason: None,
        sandbox: ExecutionBackendStatus::Supported,
        sandbox_reason: None,
        vm: ExecutionBackendStatus::Unsupported,
        vm_reason: Some("KVM unavailable".into()),
        runtimes: [("sandbox".to_string(), "runsc".to_string())]
            .into_iter()
            .collect(),
        checked_at: Some("2026-10-22T00:00:00Z".into()),
    };
    let value = serde_json::to_value(&capabilities).unwrap();
    assert_eq!(value["container"], "supported");
    assert_eq!(value["sandbox"], "supported");
    assert_eq!(value["vm"], "unsupported");
    assert_eq!(value["vm_reason"], "KVM unavailable");
    assert_eq!(value["runtimes"]["sandbox"], "runsc");
    let decoded: RunnerExecutionCapabilities = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, capabilities);
}

#[test]
fn capabilities_from_older_runner_without_sandbox_tier_default_to_unknown() {
    let decoded: RunnerExecutionCapabilities = serde_json::from_str(
        r#"{"container":"supported","vm":"unsupported","vm_reason":"no kvm","checked_at":"2026-10-22T00:00:00Z"}"#,
    )
    .unwrap();
    assert_eq!(decoded.sandbox, ExecutionBackendStatus::Unknown);
    assert!(decoded.sandbox_reason.is_none());
    assert!(decoded.runtimes.is_empty());
    let value = serde_json::to_value(&decoded).unwrap();
    assert!(
        value.get("runtimes").is_none(),
        "empty runtime map is omitted on the wire"
    );
}

#[test]
fn execution_mode_tier_names_round_trip_and_order_by_isolation_level() {
    for mode in RunnerExecutionMode::ALL {
        assert_eq!(RunnerExecutionMode::parse(mode.as_str()), Some(mode));
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, format!("\"{}\"", mode.as_str()));
    }
    assert_eq!(RunnerExecutionMode::parse("gvisor"), None);
    let levels: Vec<u8> = RunnerExecutionMode::ALL
        .iter()
        .map(|mode| mode.isolation_level())
        .collect();
    assert_eq!(levels, vec![0, 1, 2, 3]);
    assert!(!RunnerExecutionMode::Host.is_sandboxed());
    assert!(RunnerExecutionMode::Sandbox.is_sandboxed());
}

#[test]
fn backend_accessors_cover_every_sandboxed_tier_and_skip_host() {
    let mut capabilities = RunnerExecutionCapabilities::default();
    assert!(capabilities.backend(RunnerExecutionMode::Host).is_none());
    for mode in RunnerExecutionMode::ALL {
        capabilities.set_backend(
            mode,
            ExecutionBackendStatus::Unsupported,
            Some(format!("{} down", mode.as_str())),
        );
    }
    for mode in RunnerExecutionMode::ALL
        .into_iter()
        .filter(|m| m.is_sandboxed())
    {
        let (status, reason) = capabilities.backend(mode).unwrap();
        assert_eq!(status, ExecutionBackendStatus::Unsupported);
        assert_eq!(reason, Some(format!("{} down", mode.as_str()).as_str()));
    }
    assert_eq!(capabilities.sandbox_reason.as_deref(), Some("sandbox down"));
}
