use super::{hash_password, verify_password};

#[test]
fn hash_and_verify_roundtrip() {
    let hash = hash_password("secret-pass").expect("hash");
    assert!(verify_password("secret-pass", &hash));
    assert!(!verify_password("wrong", &hash));
}
