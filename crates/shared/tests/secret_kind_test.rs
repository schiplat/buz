//! Secret kind helpers and protocol fields (`generic` | `registry`).

use shared::protocol::{CreateSecretRequest, UpdateSecretRequest};
use shared::registry_auth::looks_like_registry_secret_value;
use shared::secret_kind::{
    is_registry_kind, normalize_secret_kind, validate_secret_name, SECRET_KIND_GENERIC,
    SECRET_KIND_REGISTRY,
};

#[test]
fn normalize_secret_kind_defaults_and_aliases() {
    assert_eq!(normalize_secret_kind(None), SECRET_KIND_GENERIC);
    assert_eq!(normalize_secret_kind(Some("")), SECRET_KIND_GENERIC);
    assert_eq!(normalize_secret_kind(Some("generic")), SECRET_KIND_GENERIC);
    assert_eq!(
        normalize_secret_kind(Some("REGISTRY")),
        SECRET_KIND_REGISTRY
    );
    assert_eq!(
        normalize_secret_kind(Some("registry")),
        SECRET_KIND_REGISTRY
    );
    assert_eq!(normalize_secret_kind(Some("unknown")), SECRET_KIND_GENERIC);
}

#[test]
fn is_registry_kind_true_only_for_registry() {
    assert!(!is_registry_kind("generic"));
    assert!(is_registry_kind("registry"));
    assert!(is_registry_kind("Registry"));
}

#[test]
fn looks_like_registry_secret_value_requires_user_and_pass() {
    assert!(looks_like_registry_secret_value(
        r#"{"username":"u","password":"p","registry":"cr.example.com"}"#
    ));
    assert!(looks_like_registry_secret_value(
        r#"{"provider":"acr_personal","username":"u","password":"p"}"#
    ));
    assert!(!looks_like_registry_secret_value(r#"{"username":"u"}"#));
    assert!(!looks_like_registry_secret_value("plain-password"));
    assert!(!looks_like_registry_secret_value(""));
}

#[test]
fn create_secret_kind_round_trip() {
    let req: CreateSecretRequest =
        serde_json::from_str(r#"{"name":"acr","value":"{}","team_id":"t1","kind":"registry"}"#)
            .expect("parse");
    assert_eq!(req.kind.as_deref(), Some("registry"));

    let bare: CreateSecretRequest =
        serde_json::from_str(r#"{"name":"X","value":"v","team_id":"t1"}"#).expect("parse");
    assert_eq!(bare.kind, None);
}

#[test]
fn update_secret_kind_optional() {
    let req: UpdateSecretRequest =
        serde_json::from_str(r#"{"value":"","kind":"generic"}"#).expect("parse");
    assert_eq!(req.kind.as_deref(), Some("generic"));
}

#[test]
fn validate_secret_name_accepts_upper_snake_case() {
    for name in [
        "POSTGRES_PASSWORD",
        "AFDIAN_API_TOKEN",
        "_PRIVATE",
        "GIT_SSH_KEY",
        "A1_B2",
    ] {
        assert!(validate_secret_name(name).is_ok(), "should accept {name}");
    }
}

#[test]
fn validate_secret_name_rejects_lowercase_and_invalid() {
    for name in [
        "pg_password",
        "afdianApiToken",
        "afdian-api-token",
        "1START",
        "has space",
        "UPPER lower",
        "",
    ] {
        assert!(
            validate_secret_name(name).is_err(),
            "should reject {name:?}"
        );
    }
}

#[test]
fn validate_secret_name_trims_whitespace() {
    assert!(validate_secret_name("  POSTGRES_PASSWORD  ").is_ok());
}

#[test]
fn secret_protocol_no_longer_has_env_name() {
    // Legacy clients may still send env_name; serde ignores unknown fields by
    // default, so create/update must not fail and must not carry the field.
    let req: CreateSecretRequest =
        serde_json::from_str(r#"{"name":"PG_PASSWORD","value":"v","team_id":"t1","env_name":"X"}"#)
            .expect("parse with unknown env_name ignored");
    assert_eq!(req.name, "PG_PASSWORD");
    assert_eq!(req.kind, None);
}
