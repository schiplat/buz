use super::*;

fn keypair() -> (String, String) {
    generate_keypair().expect("keypair")
}

#[test]
fn sign_and_verify_roundtrip() {
    let (secret, public) = keypair();
    let index = br#"{"steps":[{"id":"buz/hello","version":"1.0.0"}]}"#;
    let sig = sign_document(index, &secret, "test-key").expect("sign");
    assert_eq!(
        verify_document(index, &sig, &public, SIGNATURE_FILE).expect("verify"),
        "test-key"
    );
    assert!(sig.contains("\"alg\": \"ed25519\""));
}

#[test]
fn rejects_a_tampered_document() {
    let (secret, public) = keypair();
    let index = br#"{"steps":[]}"#;
    let sig = sign_document(index, &secret, "k").expect("sign");
    let tampered = br#"{"steps":[{"id":"evil/x","version":"1.0.0"}]}"#;
    let error = verify_document(tampered, &sig, &public, SIGNATURE_FILE).unwrap_err();
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
    let sig = sign_document(index, &secret, "k").expect("sign");
    let error = verify_document(index, &sig, &other_public, SIGNATURE_FILE).unwrap_err();
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
    let sig = sign_document(index, &secret, "k").unwrap();

    assert!(verify_document(index, "not json", &public, SIGNATURE_FILE)
        .unwrap_err()
        .contains("not a signature document"));
    assert!(verify_document(index, &sig, "not-base64!", SIGNATURE_FILE)
        .unwrap_err()
        .contains("public key is not valid base64"));
    let mut wrong_alg: serde_json::Value = serde_json::from_str(&sig).unwrap();
    wrong_alg["alg"] = "rsa".into();
    assert!(
        verify_document(index, &wrong_alg.to_string(), &public, SIGNATURE_FILE)
            .unwrap_err()
            .contains("unsupported signature algorithm")
    );
}

/// The review ledger is verified with the same envelope under its own file
/// name — a ledger failure must not read as an index failure.
#[test]
fn reviews_ledger_uses_its_own_signature_file_name() {
    let (secret, public) = keypair();
    let ledger = br#"{"reviews":[{"sha256":"aa"}]}"#;
    let sig = sign_document(ledger, &secret, "k").expect("sign");
    assert_eq!(
        verify_document(ledger, &sig, &public, REVIEWS_SIGNATURE_FILE).expect("verify"),
        "k"
    );
    let tampered = br#"{"reviews":[]}"#;
    let error = verify_document(tampered, &sig, &public, REVIEWS_SIGNATURE_FILE).unwrap_err();
    assert!(error.contains("different revisions"), "{error}");
}
