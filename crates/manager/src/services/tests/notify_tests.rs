//! Unit tests for notify helpers (Feishu card + should-send + job URL).

use crate::services::notify::{build_feishu_card, job_detail_url, NotifyEvent};
use shared::pipeline_yaml::{notify_should_send, DEFAULT_NOTIFY_ON};

#[test]
fn default_on_excludes_cancelled() {
    assert!(notify_should_send(None, "success"));
    assert!(notify_should_send(None, "failed"));
    assert!(notify_should_send(None, "timeout")); // maps to failure
    assert!(!notify_should_send(None, "cancelled"));
    assert!(!notify_should_send(None, "running"));
    assert_eq!(DEFAULT_NOTIFY_ON, &["success", "failure"]);
}

#[test]
fn explicit_on_includes_cancelled() {
    let on = vec!["success".into(), "cancelled".into()];
    assert!(notify_should_send(Some(&on), "success"));
    assert!(!notify_should_send(Some(&on), "failed"));
    assert!(notify_should_send(Some(&on), "cancelled"));
}

#[test]
fn empty_on_never_sends() {
    let on: Vec<String> = vec![];
    assert!(!notify_should_send(Some(&on), "success"));
    assert!(!notify_should_send(Some(&on), "failed"));
}

#[test]
fn job_detail_url_trims_and_joins() {
    assert_eq!(
        job_detail_url("https://buz.example.com/", "abc-123"),
        Some("https://buz.example.com/jobs/abc-123".into())
    );
    assert_eq!(
        job_detail_url("https://buz.example.com", "abc-123"),
        Some("https://buz.example.com/jobs/abc-123".into())
    );
    assert_eq!(job_detail_url("", "abc"), None);
    assert_eq!(job_detail_url("   ", "abc"), None);
}

fn sample_event(job_url: Option<&str>) -> NotifyEvent {
    NotifyEvent {
        pipeline_name: "demo".into(),
        pipeline_id: "p1".into(),
        job_id: "job-abc".into(),
        status: "success".into(),
        trigger_by: "alice".into(),
        duration_secs: Some(125),
        job_url: job_url.map(str::to_string),
    }
}

#[test]
fn feishu_card_success_shape() {
    let card = build_feishu_card(&sample_event(None));
    assert_eq!(card["msg_type"], "interactive");
    assert_eq!(card["card"]["header"]["template"], "green");
    assert_eq!(
        card["card"]["header"]["title"]["content"],
        "Pipeline succeeded"
    );
    let fields = card["card"]["elements"][0]["fields"]
        .as_array()
        .expect("fields");
    let joined = fields
        .iter()
        .filter_map(|f| f["text"]["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("demo"));
    assert!(joined.contains("job-abc"));
    assert!(joined.contains("2m 5s"));
    assert!(joined.contains("alice"));
    // No View Job button when job_url is absent
    assert_eq!(card["card"]["elements"].as_array().unwrap().len(), 1);
}

#[test]
fn feishu_card_includes_view_job_button() {
    let url = "https://buz.example.com/jobs/job-abc";
    let card = build_feishu_card(&sample_event(Some(url)));
    let elements = card["card"]["elements"].as_array().unwrap();
    assert_eq!(elements.len(), 2);
    assert_eq!(elements[1]["tag"], "action");
    assert_eq!(elements[1]["actions"][0]["url"], url);
    assert_eq!(elements[1]["actions"][0]["text"]["content"], "View Job");
}

#[test]
fn feishu_card_failed_template() {
    let event = NotifyEvent {
        pipeline_name: "x".into(),
        pipeline_id: "p".into(),
        job_id: "j".into(),
        status: "failed".into(),
        trigger_by: "cron".into(),
        duration_secs: None,
        job_url: None,
    };
    let card = build_feishu_card(&event);
    assert_eq!(card["card"]["header"]["template"], "red");
}
