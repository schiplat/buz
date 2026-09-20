use shared::protocol::SystemInfo;

/// Minimal payload as an **older** Runner sends it (no `clock_skew_seconds`).
fn legacy_system_info_json() -> serde_json::Value {
    serde_json::json!({
        "runner_version": "1.5.6",
        "hostname": "runner-01",
        "os": "Debian GNU/Linux 12",
        "kernel_version": "6.1.0",
        "cpu_arch": "x86_64",
        "cpu_cores": 4,
        "cpu_model": "Intel",
        "cpu_usage_percent": 3.5,
        "memory_total_mb": 8192,
        "memory_used_mb": 1024,
        "swap_total_mb": 0,
        "swap_used_mb": 0,
        "disk_total_gb": 40.0,
        "disk_used_gb": 12.0,
        "network_interfaces": [],
        "ip_addresses": ["10.0.0.5"],
        "uptime_seconds": 600
    })
}

#[test]
fn system_info_without_clock_skew_deserializes() {
    let info: SystemInfo =
        serde_json::from_value(legacy_system_info_json()).expect("parse legacy payload");
    assert_eq!(
        info.clock_skew_seconds, None,
        "missing clock_skew_seconds must default to None (older Runner)"
    );
}

#[test]
fn system_info_carries_runner_clock_skew() {
    // Positive = the Runner clock is behind the Manager.
    let mut payload = legacy_system_info_json();
    payload["clock_skew_seconds"] = serde_json::json!(45);
    let info: SystemInfo = serde_json::from_value(payload).expect("parse skew payload");
    assert_eq!(info.clock_skew_seconds, Some(45));

    let json = serde_json::to_value(&info).expect("serialize");
    assert_eq!(
        json["clock_skew_seconds"], 45,
        "skew is sent to the Manager"
    );
}

#[test]
fn system_info_serializes_absent_skew_as_null() {
    let info: SystemInfo = serde_json::from_value(legacy_system_info_json()).expect("parse");
    let json = serde_json::to_value(&info).expect("serialize");
    assert!(
        json["clock_skew_seconds"].is_null(),
        "no measurement → null, not a bogus 0"
    );
}
