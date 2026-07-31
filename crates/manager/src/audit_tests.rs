use super::{redact_value, secret_snapshot};
use serde_json::json;

#[test]
fn redact_nested_password_and_value() {
    let v = json!({
        "name": "acr",
        "value": "super-secret-password",
        "nested": { "password": "hunter2", "registry": "cr.example.com" }
    });
    let r = redact_value(&v);
    assert_eq!(r["name"], "acr");
    assert_ne!(r["value"], "super-secret-password");
    assert!(r["value"].as_str().unwrap().contains("****"));
    assert_ne!(r["nested"]["password"], "hunter2");
    assert_eq!(r["nested"]["registry"], "cr.example.com");
}

#[test]
fn secret_snapshot_never_has_raw_secret() {
    let s = secret_snapshot("id1", "S", "slug", "tid", false, "generic");
    assert_eq!(s["value"], "***");
    assert_eq!(s["name"], "S");
}

#[test]
fn redact_credential_data() {
    let v = json!({ "name": "h1", "credential_data": "BEGIN PRIVATE KEY" });
    let r = redact_value(&v);
    assert_ne!(r["credential_data"], "BEGIN PRIVATE KEY");
}
