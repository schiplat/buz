use super::*;

#[test]
fn round_trip() {
    let secret = "test-secret-key-for-encryption";
    let original = "-----BEGIN OPENSSH PRIVATE KEY-----\nthis-is-a-test-key\n-----END OPENSSH PRIVATE KEY-----";

    let encrypted = encrypt(original, secret).unwrap();
    assert!(!encrypted.is_empty());
    assert_ne!(
        encrypted, original,
        "encrypted value should differ from plaintext"
    );

    let decrypted = decrypt(&encrypted, secret).unwrap();
    assert_eq!(decrypted, original);
}

#[test]
fn empty_returns_empty() {
    let secret = "test-secret";
    assert_eq!(encrypt("", secret).unwrap(), "");
    assert_eq!(decrypt("", secret).unwrap(), "");
}

#[test]
fn plaintext_is_rejected() {
    let secret = "test-secret";
    let err = decrypt("my-plaintext-password", secret).unwrap_err();
    assert!(
        err.contains("not encrypted") || err.contains("re-save"),
        "{err}"
    );
}

#[test]
fn wrong_secret_fails() {
    let secret_a = "secret-a";
    let secret_b = "secret-b";
    let original = "super-sensitive-data";

    let encrypted = encrypt(original, secret_a).unwrap();
    let result = decrypt(&encrypted, secret_b);
    assert!(result.is_err(), "decrypt with wrong secret should fail");
}

#[test]
fn registry_json_secret_round_trip() {
    let jwt = "manager-jwt-secret";
    let json =
        r#"{"provider":"acr_personal","registry":"crpi.example","username":"u","password":"p"}"#;
    let encrypted = encrypt(json, jwt).unwrap();
    assert!(!encrypted.contains("password"));
    assert_eq!(decrypt(&encrypted, jwt).unwrap(), json);
}

#[test]
fn mask_string_short_fully_hidden() {
    assert_eq!(mask_string("ab"), "****");
    assert_eq!(mask_string("abcd"), "****");
}

#[test]
fn mask_string_keeps_head_tail() {
    assert_eq!(mask_string("password12"), "pa****12");
    assert_eq!(mask_string("hello"), "h****o");
}

#[test]
fn mask_for_display_redacts_json_password() {
    let raw = r#"{"provider":"acr_personal","registry":"crpi.example","username":"aliyun","password":"SuperSecret99"}"#;
    let masked = mask_for_display(raw);
    assert!(masked.contains("\"provider\":\"acr_personal\""), "{masked}");
    assert!(masked.contains("\"username\":\"aliyun\""), "{masked}");
    assert!(masked.contains("\"registry\":\"crpi.example\""), "{masked}");
    assert!(!masked.contains("SuperSecret99"), "{masked}");
    assert!(masked.contains("****"), "{masked}");
}

#[test]
fn mask_for_display_plain_string() {
    let masked = mask_for_display("SuperSecret99");
    assert_eq!(masked, "Su****99");
}
