use super::{required_labels_for_job, runner_satisfies_labels};

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
