use manager::password::hash_password;
use manager::password::validate_password_complexity;
use manager::password::verify_password;

#[test]
fn hash_and_verify_roundtrip() {
    let hash = hash_password("secret-pass").expect("hash");
    assert!(verify_password("secret-pass", &hash));
    assert!(!verify_password("wrong", &hash));
}

#[test]
fn verify_rejects_non_argon2_blob() {
    // Secrets use AES ciphertext — must never be accepted as a user password hash.
    assert!(!verify_password("anything", "not-a-phc-string"));
    assert!(!verify_password("anything", "aGVsbG8="));
}

#[test]
fn complexity_rejects_short() {
    assert!(validate_password_complexity("Ab1!xyz").is_err());
}

#[test]
fn complexity_rejects_missing_classes() {
    assert!(validate_password_complexity("alllowercase1!").is_err());
    assert!(validate_password_complexity("ALLUPPERCASE1!").is_err());
    assert!(validate_password_complexity("NoDigitsHere!").is_err());
    assert!(validate_password_complexity("NoSymbolHere1").is_err());
}

#[test]
fn complexity_rejects_whitespace() {
    assert!(validate_password_complexity("Abcd 123!").is_err());
}

#[test]
fn complexity_accepts_strong() {
    assert!(validate_password_complexity("Str0ng!Pass").is_ok());
}
