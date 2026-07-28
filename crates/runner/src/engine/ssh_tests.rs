use super::{
    build_rsa_fallback_chain, format_agent_identity_label, hash_label, key_type_display,
    resolve_known_hosts_path, verify_against_known_hosts,
};
use russh::keys::{load_secret_key, HashAlg};

/// Unencrypted Ed25519 OpenSSH private key (test fixture, not a real host key).
const ED25519_PLAIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACBAX8FqU8zwcAYag9bFm0TFLAjS5cRQSibSE85+76iJ/QAAAJDtZ2rw7Wdq
8AAAAAtzc2gtZWQyNTUxOQAAACBAX8FqU8zwcAYag9bFm0TFLAjS5cRQSibSE85+76iJ/Q
AAAEB3KgQLK5t1vDCnEAXhmKOBqxfXYKHfbmGIMz02WHVN8EBfwWpTzPBwBhqD1sWbRMUs
CNLlxFBKJtITzn7vqIn9AAAACGJ1ei10ZXN0AQIDBAU=
-----END OPENSSH PRIVATE KEY-----
";

/// Encrypted Ed25519 key — passphrase: `test-passphrase`
const ED25519_ENCRYPTED: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABC4UEGI+M
X5ADDLTP+68hx8AAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIFqnfiL+/wi1S0Oh
F0XCoiIOcU8JQICPmhPGBvyZiGHQAAAAkIsKDdDJPDFaUAOQC7E+I0k/d3bDcJbnNRuM46
A5TZwBvpVAqxnhHNqmCewLpAmvtodU7UMzrZ3hi4ttc4JqxE9wt7CLvcisT3/yR+c0YSwE
JbMA3ZeF0viShHWstnloRMz2LF+cMGar/0XbMFjDLud/Lqijlb5HyjR59s99OJi99T+CHW
52v3h8WpTixPVwpQ==
-----END OPENSSH PRIVATE KEY-----
";

/// Unencrypted RSA OpenSSH private key (test fixture).
const RSA_PLAIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAABFwAAAAdzc2gtcn
NhAAAAAwEAAQAAAQEApP7Ay5fSMtklsWCeff42fOVgEuOVUJUWmEfpBPgmSOftCMBbN8na
6KlE3z/UNwOMaC/2fFCwpZ23k6gBcn/U3315qjEZzaaXed3WTVHQA7f7+39ur9f3mq8wNE
CgR5UhbxwGqrd3Id8T4ou1KBZHMvGSp9fuKV3IrGOkvWtAYs1oL6TNhKjUM7h3y6CdQ8Ad
X5JnF31Udw+hqV2KlRv9Ta3W6GGlEdTOOa2Lom7ixJJHjO8VufFFGx6vjm2+sOYcW5bghf
fTbXqGGNNL1ys17/r/EdH8yEwRhaYVq5liuUPbiz2nzCvH/OpdHUx2YpEgteKBxazKDtan
7GJLmSj5HQAAA8idqcaFnanGhQAAAAdzc2gtcnNhAAABAQCk/sDLl9Iy2SWxYJ59/jZ85W
AS45VQlRaYR+kE+CZI5+0IwFs3ydroqUTfP9Q3A4xoL/Z8ULClnbeTqAFyf9TffXmqMRnN
ppd53dZNUdADt/v7f26v1/earzA0QKBHlSFvHAaqt3ch3xPii7UoFkcy8ZKn1+4pXcisY6
S9a0BizWgvpM2EqNQzuHfLoJ1DwB1fkmcXfVR3D6GpXYqVG/1NrdboYaUR1M45rYuibuLE
kkeM7xW58UUbHq+Obb6w5hxbluCF99NteoYY00vXKzXv+v8R0fzITBGFphWrmWK5Q9uLPa
fMK8f86l0dTHZikSC14oHFrMoO1qfsYkuZKPkdAAAAAwEAAQAAAQB7hoJ+qi5jhTD0CDsA
ZtcSUIPKAHqkNsoQvh2Va9b11DUgzSo9rTDn6Po4DchqW4y939KwpZRqmZZ7xFc5Cr0riP
wST+pajbNwOIYdoveSISdU1cXfflfQhgIXzJ5ymRLL5XaUTwCw6hswC5pX5b3uRO+gG0gh
ziXT8lG+wlIU5TGcCNuswJYbC0LCG0NbUWDEe38s3nkKzGpuIxHqPezeO/rHrmGLCLWRZm
j7NUfnEMKrU6zbEw5QZgQKRNhg6mEKS9DPhLaW2uO8C5oLPgIyU24uCufj2Z5ewgJX8Rme
2iFCFgP+zhF+eg6PqvW+3aMKpd0J3tQ6BeEr0biNrbbdAAAAgE73SWRSUuy7WHyU6EXlh5
IwmZ4x9TN3mlutji/roFDCtllEXxWih6xWkpFXsoWdGKBEh5jSfZyX8kjqo8+WuUdAUdJw
c/xX2y5YnN5xjkW7V9xMvIIMpzobZP8/Dq1TkIDGrLeBIFngPPAXiegZZZDyF3pg/DvA/s
KVw4N6mjMqAAAAgQDSo8NiX7YJPmFrxVV4aLwjtjvS0JpiIUzE9FVJcXftvE17quu01gRf
UNMn+mT9p6QwwbkglOaFQtlW3uh3cGpK1NUUrOjVBvFgt8P3k3TAzvEkd3YtpYuCGyP6O9
L2wZb2jM5ASt4NevNq+lum9MUR2SWQp8mF+rDIhWzmb6nSswAAAIEAyIatGn5BlC5YY5Ai
6KxN1FidpkU3/lLU7JYoQ5L+uyxBZ9z3VTuxYAiiJv+6nQp6LUqfO0UmwAO98BO9goj/qq
mUbyacGjJfib1KAWZfbRuf5era1h491o49ywNmDjftAeXpQpzTcrNd7hV4rcbH3mu93fQ5
5d79pOoxAQI/rO8AAAAMYnV6LXRlc3QtcnNhAQIDBAUGBw==
-----END OPENSSH PRIVATE KEY-----
";

fn load_and_cleanup(
    pem: &str,
    passphrase: Option<&str>,
) -> Result<russh::keys::PrivateKey, russh::keys::Error> {
    let path = std::env::temp_dir().join(format!("buz_test_key_{}", uuid::Uuid::new_v4()));
    std::fs::write(&path, pem).expect("write temp key");
    let result = load_secret_key(&path, passphrase);
    let _ = std::fs::remove_file(&path);
    result
}

#[test]
fn rsa_fallback_chain_deduplicates() {
    let chain = build_rsa_fallback_chain(Some(HashAlg::Sha512));
    assert_eq!(
        chain.len(),
        3,
        "Sha512+Sha256+None = 3 (Sha512 deduplicated)"
    );
    assert_eq!(chain[0], Some(HashAlg::Sha512));
    assert_eq!(chain[1], Some(HashAlg::Sha256));
    assert_eq!(chain[2], None);
}

#[test]
fn rsa_fallback_chain_no_preference() {
    let chain = build_rsa_fallback_chain(None);
    assert_eq!(chain.len(), 3, "Sha512, Sha256, None");
    assert_eq!(chain[0], Some(HashAlg::Sha512));
    assert_eq!(chain[1], Some(HashAlg::Sha256));
    assert_eq!(chain[2], None);
}

#[test]
fn rsa_fallback_chain_prefers_sha256() {
    let chain = build_rsa_fallback_chain(Some(HashAlg::Sha256));
    assert_eq!(chain.len(), 3);
    assert_eq!(chain[0], Some(HashAlg::Sha256));
    assert_eq!(chain[1], Some(HashAlg::Sha512));
    assert_eq!(chain[2], None);
}

#[test]
fn hash_label_covers_known_variants() {
    assert_eq!(hash_label(&Some(HashAlg::Sha512)), "rsa-sha2-512");
    assert_eq!(hash_label(&Some(HashAlg::Sha256)), "rsa-sha2-256");
    assert_eq!(hash_label(&None), "ssh-rsa");
}

#[test]
fn load_unencrypted_ed25519_key() {
    let key = load_and_cleanup(ED25519_PLAIN, None).expect("ed25519 plain key should load");
    assert_eq!(key_type_display(&key.algorithm()), "Ed25519");
    let _fp = key.fingerprint(HashAlg::Sha256);
}

#[test]
fn load_unencrypted_rsa_key() {
    let key = load_and_cleanup(RSA_PLAIN, None).expect("rsa plain key should load");
    assert_eq!(key_type_display(&key.algorithm()), "RSA");
    assert!(key.algorithm().clone().is_rsa());
}

#[test]
fn load_encrypted_ed25519_with_passphrase() {
    let key = load_and_cleanup(ED25519_ENCRYPTED, Some("test-passphrase"))
        .expect("encrypted ed25519 should load with correct passphrase");
    assert_eq!(key_type_display(&key.algorithm()), "Ed25519");
}

#[test]
fn load_encrypted_ed25519_wrong_passphrase_fails() {
    let err = load_and_cleanup(ED25519_ENCRYPTED, Some("wrong-passphrase"));
    assert!(err.is_err(), "wrong passphrase must fail");
}

#[test]
fn load_encrypted_ed25519_missing_passphrase_fails() {
    let err = load_and_cleanup(ED25519_ENCRYPTED, None);
    assert!(err.is_err(), "encrypted key without passphrase must fail");
}

#[test]
fn load_public_key_material_fails() {
    let pub_only = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEBfwWpTzPBwBhqD1sWbRMUsCNLlxFBKJtITzn7vqIn9 buz-test\n";
    let err = load_and_cleanup(pub_only, None);
    assert!(err.is_err(), "public key must not load as private key");
}

#[test]
fn agent_identity_label_uses_comment_or_index() {
    assert_eq!(format_agent_identity_label(0, ""), "#1");
    assert_eq!(format_agent_identity_label(2, ""), "#3");
    assert_eq!(format_agent_identity_label(0, "user@laptop"), "user@laptop");
}

#[test]
fn resolve_known_hosts_path_prefers_configured() {
    let p = resolve_known_hosts_path(Some("/tmp/custom_known_hosts")).unwrap();
    assert_eq!(p, std::path::PathBuf::from("/tmp/custom_known_hosts"));
}

#[test]
fn known_hosts_accepts_matching_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known_hosts");
    let key = load_and_cleanup(ED25519_PLAIN, None).expect("load ed25519");
    let pubkey = key.public_key().clone();
    // Fixture private key carries comment "buz-test"; learn writes OpenSSH form.
    russh::keys::known_hosts::learn_known_hosts_path("testhost.example", 22, &pubkey, &path)
        .expect("learn host key");

    assert!(verify_against_known_hosts(
        "testhost.example",
        22,
        &pubkey,
        Some(path.to_str().unwrap()),
    )
    .unwrap());
}

#[test]
fn known_hosts_ignores_key_comment_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known_hosts");
    std::fs::write(
        &path,
        "testhost.example ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEBfwWpTzPBwBhqD1sWbRMUsCNLlxFBKJtITzn7vqIn9\n",
    )
    .unwrap();

    let key = load_and_cleanup(ED25519_PLAIN, None).expect("load ed25519");
    // Public key still has comment "buz-test" — must not trigger KeyChanged.
    assert!(verify_against_known_hosts(
        "testhost.example",
        22,
        key.public_key(),
        Some(path.to_str().unwrap()),
    )
    .unwrap());
}

#[test]
fn known_hosts_rejects_unknown_host() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known_hosts");
    std::fs::write(&path, "").unwrap();
    let key = load_and_cleanup(ED25519_PLAIN, None).expect("load ed25519");
    let pubkey = key.public_key().clone();

    let err =
        verify_against_known_hosts("unknown.example", 22, &pubkey, Some(path.to_str().unwrap()));
    assert!(err.is_err(), "unknown host must fail");
    let msg = err.unwrap_err().to_string();
    assert!(msg.contains("not found"), "{msg}");
}
