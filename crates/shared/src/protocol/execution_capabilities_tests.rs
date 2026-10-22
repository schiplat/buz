use super::{ExecutionBackendStatus, RunnerExecutionCapabilities, SystemInfo};

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
        vm: ExecutionBackendStatus::Unsupported,
        vm_reason: Some("KVM unavailable".into()),
        checked_at: Some("2026-10-22T00:00:00Z".into()),
    };
    let value = serde_json::to_value(&capabilities).unwrap();
    assert_eq!(value["container"], "supported");
    assert_eq!(value["vm"], "unsupported");
    assert_eq!(value["vm_reason"], "KVM unavailable");
    let decoded: RunnerExecutionCapabilities = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, capabilities);
}
