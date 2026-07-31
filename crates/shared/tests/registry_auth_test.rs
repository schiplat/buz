//! Tests for registry_auth helpers.

use shared::registry_auth::{parse_registry_secret_value, registry_host_from_image, RegistryAuth};

#[test]
fn registry_from_acr_personal_image() {
    assert_eq!(
        registry_host_from_image("crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/ns/app:1.0")
            .as_deref(),
        Some("crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com")
    );
}

#[test]
fn registry_from_legacy_acr() {
    assert_eq!(
        registry_host_from_image("registry.cn-hangzhou.aliyuncs.com/ns/app:latest").as_deref(),
        Some("registry.cn-hangzhou.aliyuncs.com")
    );
}

#[test]
fn registry_none_for_docker_hub_short() {
    assert_eq!(registry_host_from_image("alpine:3.20"), None);
    assert_eq!(registry_host_from_image("library/alpine:3.20"), None);
}

#[test]
fn registry_with_port() {
    assert_eq!(
        registry_host_from_image("localhost:5000/ns/app:1").as_deref(),
        Some("localhost:5000")
    );
}

#[test]
fn parse_full_json() {
    let raw = r#"{
      "registry": "crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com",
      "username": "aliyun_user",
      "password": "fixed-pass"
    }"#;
    let auth = parse_registry_secret_value(raw, None).unwrap();
    assert_eq!(
        auth,
        RegistryAuth {
            registry: "crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com".into(),
            username: "aliyun_user".into(),
            password: "fixed-pass".into(),
            provider: None,
        }
    );
}

#[test]
fn parse_infers_registry_from_image() {
    let raw = r#"{"username":"u","password":"p"}"#;
    let auth = parse_registry_secret_value(
        raw,
        Some("crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/ns/app:1"),
    )
    .unwrap();
    assert_eq!(
        auth.registry,
        "crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com"
    );
}

#[test]
fn parse_rejects_missing_registry_without_image() {
    let err = parse_registry_secret_value(r#"{"username":"u","password":"p"}"#, None).unwrap_err();
    assert!(err.contains("registry"), "{err}");
}

#[test]
fn parse_rejects_invalid_json() {
    let err = parse_registry_secret_value("not-json", None).unwrap_err();
    assert!(err.contains("JSON"), "{err}");
}

#[test]
fn parse_rejects_empty_username() {
    let err = parse_registry_secret_value(
        r#"{"username":"","password":"p","registry":"r.example"}"#,
        None,
    )
    .unwrap_err();
    assert!(err.contains("username"), "{err}");
}

#[test]
fn parse_provider_acr_personal() {
    let raw = r#"{
      "provider": "acr_personal",
      "registry": "crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com",
      "username": "u",
      "password": "p"
    }"#;
    let auth = parse_registry_secret_value(raw, None).unwrap();
    assert_eq!(auth.provider.as_deref(), Some("acr_personal"));
}
