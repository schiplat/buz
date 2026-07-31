//! Git webhook verify + normalize unit tests.

use shared::git_webhook::{
    branch_matches, has_skip_ci, parse_gitee_event, parse_github_event, resolve_delivery_id,
    verify_gitee_token, verify_github_signature, NormalizedGitEvent,
};

#[test]
fn github_signature_roundtrip() {
    let secret = "topsecret";
    let body = br#"{"ref":"refs/heads/main"}"#;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(body);
    let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    assert!(verify_github_signature(secret, body, &sig));
    assert!(!verify_github_signature(secret, body, "sha256=deadbeef"));
    assert!(!verify_github_signature("other", body, &sig));
}

#[test]
fn gitee_token_compare() {
    assert!(verify_gitee_token("abc", Some("abc")));
    assert!(!verify_gitee_token("abc", Some("abd")));
    assert!(!verify_gitee_token("abc", None));
}

#[test]
fn parse_github_push() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{
          "ref": "refs/heads/main",
          "after": "abcdef0123456789",
          "head_commit": {"id": "abcdef0123456789", "message": "feat: hi\n\nbody"},
          "repository": {"full_name": "org/app", "ssh_url": "git@github.com:org/app.git"},
          "sender": {"login": "alice"},
          "compare": "https://github.com/org/app/compare/1...2"
        }"#,
    )
    .unwrap();
    let ev = parse_github_event("push", &body).unwrap();
    assert_eq!(ev.branch, "main");
    assert_eq!(ev.commit_sha, "abcdef0123456789");
    assert_eq!(ev.commit_message, "feat: hi");
    assert_eq!(ev.repo_full_name, "org/app");
    let inputs = ev.to_inputs("github");
    assert_eq!(
        inputs.get("GIT_SHA_SHORT").map(String::as_str),
        Some("abcdef0")
    );
    assert_eq!(inputs.get("GIT_BRANCH").map(String::as_str), Some("main"));
}

#[test]
fn parse_github_ping() {
    let body: serde_json::Value =
        serde_json::from_str(r#"{"repository":{"full_name":"org/app"}}"#).unwrap();
    let ev = parse_github_event("ping", &body).unwrap();
    assert!(ev.is_ping);
}

#[test]
fn parse_gitee_push() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{
          "ref": "refs/heads/develop",
          "after": "1234567890abcdef",
          "commits": [{"id": "1234567890abcdef", "message": "fix: [skip ci] typo"}],
          "repository": {"path_with_namespace": "team/demo", "git_ssh_url": "git@gitee.com:team/demo.git"},
          "sender": {"login": "bob", "name": "Bob"}
        }"#,
    )
    .unwrap();
    let ev = parse_gitee_event("Push Hook", &body).unwrap();
    assert_eq!(ev.branch, "develop");
    assert_eq!(ev.repo_full_name, "team/demo");
    assert!(has_skip_ci(&ev.commit_message));
}

#[test]
fn branch_filter_globs() {
    assert!(branch_matches("", "main"));
    assert!(branch_matches("main", "main"));
    assert!(!branch_matches("main", "develop"));
    assert!(branch_matches("release/*", "release/1.0"));
    assert!(branch_matches("main,develop", "develop"));
    assert!(branch_matches("*feat*", "my-feature"));
}

#[test]
fn gitee_delivery_id_is_stable_composite_not_timestamp() {
    let ev = NormalizedGitEvent {
        event_type: "push".into(),
        repo_full_name: "team/demo".into(),
        git_ref: "refs/heads/master".into(),
        commit_sha: "abcdef0123456789".into(),
        ..Default::default()
    };
    let a = resolve_delivery_id("gitee", Some("1710000000000"), &ev).unwrap();
    let b = resolve_delivery_id("gitee", Some("1710000000999"), &ev).unwrap();
    assert_eq!(a, b);
    assert_eq!(a, "gitee:team/demo:refs/heads/master:abcdef0123456789");
    assert!(!a.contains("1710000000"));
}

#[test]
fn github_delivery_id_prefers_header() {
    let ev = NormalizedGitEvent {
        event_type: "push".into(),
        repo_full_name: "org/app".into(),
        git_ref: "refs/heads/main".into(),
        commit_sha: "abc".into(),
        ..Default::default()
    };
    assert_eq!(
        resolve_delivery_id("github", Some("deliv-uuid-1"), &ev).as_deref(),
        Some("deliv-uuid-1")
    );
    assert_eq!(
        resolve_delivery_id("github", None, &ev).as_deref(),
        Some("github:org/app:refs/heads/main:abc")
    );
}

#[test]
fn ping_has_no_delivery_id() {
    let ev = NormalizedGitEvent {
        is_ping: true,
        ..Default::default()
    };
    assert!(resolve_delivery_id("gitee", Some("ts"), &ev).is_none());
}
