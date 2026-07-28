use super::*;

#[test]
fn round_trip() {
    let secret = "test-secret-key-for-encryption";
    let original = "-----BEGIN OPENSSH PRIVATE KEY-----\nthis-is-a-test-key\n-----END OPENSSH PRIVATE KEY-----";

    let encrypted = encrypt(original, secret).unwrap();
    assert!(!encrypted.is_empty());
    assert_ne!(encrypted, original, "encrypted value should differ from plaintext");

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
fn legacy_plaintext_backward_compatible() {
    let secret = "test-secret";
    let legacy_value = "my-plaintext-password";
    // decrypt should return the value as-is for unencrypted legacy data
    let result = decrypt(legacy_value, secret).unwrap();
    assert_eq!(result, legacy_value);
}

#[test]
fn wrong_secret_fails() {
    let secret_a = "secret-a";
    let secret_b = "secret-b";
    let original = "super-sensitive-data";

    let encrypted = encrypt(original, secret_a).unwrap();
    // Decrypting with a different secret should fail
    let result = decrypt(&encrypted, secret_b);
    assert!(result.is_err(), "decrypt with wrong secret should fail");
}
