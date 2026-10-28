//! Unit tests for `logs.rs` — the pipeline-stream debug-message classifier.

use manager::api::logs::is_debug_status_message;

#[test]
fn debug_status_message_is_flagged() {
    let msg = r#"{"type":"status","job_id":"j1","data":{"id":"j1","status":"failed","debug_unredact":true}}"#;
    assert!(is_debug_status_message(msg));
}

#[test]
fn normal_status_message_is_not_flagged() {
    let msg = r#"{"type":"status","job_id":"j2","data":{"id":"j2","status":"success","debug_unredact":false}}"#;
    assert!(!is_debug_status_message(msg));
}

#[test]
fn message_without_debug_field_is_not_flagged() {
    let msg = r#"{"type":"status","job_id":"j3","data":{"id":"j3","status":"running"}}"#;
    assert!(!is_debug_status_message(msg));
}

#[test]
fn non_status_or_malformed_messages_are_not_flagged() {
    assert!(!is_debug_status_message(
        r#"{"type":"log","data":{"content":"hi"}}"#
    ));
    assert!(!is_debug_status_message("not json"));
    assert!(!is_debug_status_message(""));
}
