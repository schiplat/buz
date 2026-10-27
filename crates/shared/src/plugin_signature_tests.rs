use super::*;

fn keypair() -> (String, String) {
    generate_keypair().expect("keypair")
}

#[test]
fn sign_and_verify_roundtrip() {
    let (secret, public) = keypair();
    let index = br#"{"steps":[{"id":"buz/hello","version":"1.0.0"}]}"#;
    let sig = sign_index(index, &secret, "test-key").expect("sign");
    assert_eq!(
        verify_index(index, &sig, &public).expect("verify"),
        "test-key"
    );
    assert!(sig.contains("\"alg\": \"ed25519\""));
}

#[test]
fn rejects_a_tampered_index() {
    let (secret, public) = keypair();
    let index = br#"{"steps":[]}"#;
    let sig = sign_index(index, &secret, "k").expect("sign");
    let tampered = br#"{"steps":[{"id":"evil/x","version":"1.0.0"}]}"#;
    let error = verify_index(tampered, &sig, &public).unwrap_err();
    assert!(
        error.contains("does not match its signature") && error.contains("different revisions"),
        "{error}"
    );
}

#[test]
fn rejects_a_signature_from_another_key() {
    let (secret, _) = keypair();
    let (_, other_public) = keypair();
    let index = br#"{"steps":[]}"#;
    let sig = sign_index(index, &secret, "k").expect("sign");
    let error = verify_index(index, &sig, &other_public).unwrap_err();
    assert!(
        error.contains("does not match the configured public key"),
        "{error}"
    );
}

#[test]
fn public_key_derivation_matches_generated_pair() {
    let (secret, public) = keypair();
    assert_eq!(public_key_base64(&secret).unwrap(), public);
}

#[test]
fn rejects_malformed_inputs() {
    let (secret, public) = keypair();
    let index = br#"{}"#;
    let sig = sign_index(index, &secret, "k").unwrap();

    assert!(verify_index(index, "not json", &public)
        .unwrap_err()
        .contains("not a signature document"));
    assert!(verify_index(index, &sig, "not-base64!")
        .unwrap_err()
        .contains("public key is not valid base64"));
    let mut wrong_alg: serde_json::Value = serde_json::from_str(&sig).unwrap();
    wrong_alg["alg"] = "rsa".into();
    assert!(verify_index(index, &wrong_alg.to_string(), &public)
        .unwrap_err()
        .contains("unsupported signature algorithm"));
}
