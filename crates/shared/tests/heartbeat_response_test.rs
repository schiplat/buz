use shared::protocol::HeartbeatResponse;

#[test]
fn heartbeat_response_defaults_restart_to_false() {
    // Older Managers / minimal payloads omit `restart`; serde must default it.
    let parsed: HeartbeatResponse = serde_json::from_str(r#"{"policy":{"max_concurrent_jobs":2}}"#)
        .expect("parse heartbeat without restart");
    assert!(!parsed.restart, "restart defaults to false");
    assert_eq!(parsed.policy.max_concurrent_jobs, 2);
}

#[test]
fn heartbeat_response_roundtrips_restart_flag() {
    let parsed: HeartbeatResponse =
        serde_json::from_str(r#"{"policy":{},"restart":true}"#).expect("parse restart=true");
    assert!(parsed.restart);

    let json = serde_json::to_value(&parsed).expect("serialize");
    assert_eq!(json["restart"], true, "restart field is serialized");
}
