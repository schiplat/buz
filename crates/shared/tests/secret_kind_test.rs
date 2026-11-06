//! Secret kind helpers and protocol fields (`generic` only).
//!
//! The legacy kinds were removed in v3: `registry` (with the
//! `registry_secret` kernel slot) and `aliyun_oss_auth` (with the kernel
//! `config.source` delivery). Stored rows keep their value and normalize to
//! `generic`; plugins use plain `generic` secrets instead.

use shared::protocol::{CreateSecretRequest, UpdateSecretRequest};
use shared::secret_kind::{normalize_secret_kind, validate_secret_name, SECRET_KIND_GENERIC};

#[test]
fn normalize_secret_kind_always_generic() {
    assert_eq!(normalize_secret_kind(None), SECRET_KIND_GENERIC);
    assert_eq!(normalize_secret_kind(Some("")), SECRET_KIND_GENERIC);
    assert_eq!(normalize_secret_kind(Some("generic")), SECRET_KIND_GENERIC);
    // Legacy kind values (pre-v3 rows) normalize to generic.
    assert_eq!(normalize_secret_kind(Some("registry")), SECRET_KIND_GENERIC);
    assert_eq!(
        normalize_secret_kind(Some("aliyun_oss_auth")),
        SECRET_KIND_GENERIC
    );
    assert_eq!(normalize_secret_kind(Some("unknown")), SECRET_KIND_GENERIC);
}

#[test]
fn create_secret_kind_round_trip() {
    let req: CreateSecretRequest = serde_json::from_str(
        r#"{"name":"OSS_RO","value":"{}","team_id":"t1","kind":"aliyun_oss_auth"}"#,
    )
    .expect("parse");
    assert_eq!(req.kind.as_deref(), Some("aliyun_oss_auth"));

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
