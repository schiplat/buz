use super::{
    normalize_max_concurrent, normalize_ssh_host_key_policy, required_labels_for_job,
    runner_satisfies_labels,
};
use shared::error::AppError;

#[test]
fn empty_required_matches_any_runner() {
    assert!(runner_satisfies_labels(&["macos-armx64".into()], &[]));
    assert!(runner_satisfies_labels(&[], &[]));
}

#[test]
fn requires_all_pipeline_labels() {
    let runner = vec!["linux-x64".into(), "docker".into()];
    assert!(runner_satisfies_labels(
        &runner,
        &["linux-x64".into(), "docker".into()]
    ));
    assert!(runner_satisfies_labels(&runner, &["linux-x64".into()]));
    assert!(!runner_satisfies_labels(
        &["macos-armx64".into()],
        &["linux-x64".into(), "docker".into()]
    ));
    assert!(!runner_satisfies_labels(
        &["linux-x64".into()],
        &["linux-x64".into(), "docker".into()]
    ));
}

#[test]
fn required_labels_prefer_snapshot() {
    let snap = r#"{"runner_labels":["linux-x64","docker"],"tasks":[]}"#;
    let pipe = r#"["macos-armx64"]"#;
    assert_eq!(
        required_labels_for_job(snap, pipe),
        vec!["linux-x64".to_string(), "docker".to_string()]
    );
}

#[test]
fn required_labels_fallback_to_pipeline() {
    let snap = r#"{"tasks":[]}"#;
    let pipe = r#"["linux-x64","docker"]"#;
    assert_eq!(
        required_labels_for_job(snap, pipe),
        vec!["linux-x64".to_string(), "docker".to_string()]
    );
}

#[test]
fn max_concurrent_defaults_to_one() {
    assert_eq!(normalize_max_concurrent(None).unwrap(), 1);
}

#[test]
fn max_concurrent_accepts_valid_range() {
    assert_eq!(normalize_max_concurrent(Some(1)).unwrap(), 1);
    assert_eq!(normalize_max_concurrent(Some(8)).unwrap(), 8);
    assert_eq!(normalize_max_concurrent(Some(32)).unwrap(), 32);
}

#[test]
fn max_concurrent_rejects_out_of_range() {
    assert!(matches!(
        normalize_max_concurrent(Some(0)),
        Err(AppError::ValidationFailed(_))
    ));
    assert!(matches!(
        normalize_max_concurrent(Some(33)),
        Err(AppError::ValidationFailed(_))
    ));
}

#[test]
fn ssh_host_key_policy_defaults_to_insecure() {
    assert_eq!(normalize_ssh_host_key_policy(None).unwrap(), "insecure");
}

#[test]
fn ssh_host_key_policy_accepts_known_hosts() {
    assert_eq!(
        normalize_ssh_host_key_policy(Some("known_hosts".into())).unwrap(),
        "known_hosts"
    );
}

#[test]
fn ssh_host_key_policy_rejects_unknown() {
    assert!(matches!(
        normalize_ssh_host_key_policy(Some("trust_on_first_use".into())),
        Err(AppError::ValidationFailed(_))
    ));
}
