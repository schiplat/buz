use super::{is_insecure_jwt_secret, validate_jwt_secret, MIN_JWT_SECRET_LEN};

#[test]
fn rejects_empty_and_short_secrets() {
    assert!(is_insecure_jwt_secret(""));
    assert!(is_insecure_jwt_secret("   "));
    assert!(is_insecure_jwt_secret(&"a".repeat(MIN_JWT_SECRET_LEN - 1)));
    assert!(!is_insecure_jwt_secret(&"a".repeat(MIN_JWT_SECRET_LEN)));
}

#[test]
fn rejects_known_placeholders() {
    assert!(is_insecure_jwt_secret("CHANGE_ME_USE_OPENSSL_RAND_HEX_32"));
    assert!(is_insecure_jwt_secret("changeme-demo-only"));
    assert!(is_insecure_jwt_secret("REPLACE_WITH_STRONG_SECRET___________"));
    assert!(is_insecure_jwt_secret("buz-dev-secret-change-in-production"));
}

#[test]
fn accepts_strong_looking_secret() {
    let secret = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    assert!(!is_insecure_jwt_secret(secret));
    assert!(validate_jwt_secret(secret).is_ok());
}

#[test]
fn validate_fails_without_allow_env() {
    // Ensure we do not inherit an allow flag from the parent process.
    std::env::remove_var("BUZ_ALLOW_INSECURE_JWT");
    let err = validate_jwt_secret("changeme-demo-only").unwrap_err().to_string();
    assert!(err.contains("refusing to start"), "{err}");
}
