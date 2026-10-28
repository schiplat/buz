//! Unit tests for OSS config auth parsing and signed-URL generation.

use shared::config_auth::*;

fn sample_auth() -> OssConfigAuth {
    OssConfigAuth {
        endpoint: "oss-cn-guangzhou.aliyuncs.com".to_string(),
        bucket: "example-bucket".to_string(),
        region: Some("cn-guangzhou".to_string()),
        access_key_id: "LTAI_TEST_KEY_ID".to_string(),
        access_key_secret: "test-access-key-secret".to_string(),
        provider: Some(PROVIDER_OSS_RO.to_string()),
    }
}

#[test]
fn parse_accepts_full_json() {
    let raw = r#"{
        "provider": "aliyun_oss",
        "endpoint": "oss-cn-guangzhou.aliyuncs.com",
        "bucket": "example-bucket",
        "region": "cn-guangzhou",
        "access_key_id": "LTAI123",
        "access_key_secret": "sk-secret"
    }"#;
    let auth = parse_oss_secret_value(raw).expect("valid JSON should parse");
    assert_eq!(auth.endpoint, "oss-cn-guangzhou.aliyuncs.com");
    assert_eq!(auth.bucket, "example-bucket");
    assert_eq!(auth.region.as_deref(), Some("cn-guangzhou"));
    assert_eq!(auth.access_key_id, "LTAI123");
    assert_eq!(auth.access_key_secret, "sk-secret");
    assert_eq!(auth.provider.as_deref(), Some(PROVIDER_OSS_RO));
}

#[test]
fn parse_accepts_minimal_json() {
    let raw = r#"{"endpoint":"oss-cn-hongkong.aliyuncs.com","bucket":"cfg","access_key_id":"AK","access_key_secret":"SK"}"#;
    let auth = parse_oss_secret_value(raw).expect("minimal JSON should parse");
    assert_eq!(auth.endpoint, "oss-cn-hongkong.aliyuncs.com");
    assert_eq!(auth.bucket, "cfg");
    assert!(auth.region.is_none());
    assert!(auth.provider.is_none());
}

#[test]
fn parse_rejects_missing_fields() {
    assert!(parse_oss_secret_value("").is_err(), "empty value rejected");
    assert!(
        parse_oss_secret_value("not json").is_err(),
        "non-JSON rejected"
    );
    assert!(
        parse_oss_secret_value(r#"{"endpoint":"e","access_key_id":"a","access_key_secret":"s"}"#)
            .is_err(),
        "missing bucket rejected"
    );
    assert!(
        parse_oss_secret_value(r#"{"endpoint":"e","bucket":"b","access_key_id":"a"}"#).is_err(),
        "missing access_key_secret rejected"
    );
    assert!(
        parse_oss_secret_value(r#"{"bucket":"b","access_key_id":"a","access_key_secret":"s"}"#)
            .is_err(),
        "missing endpoint rejected"
    );
}

#[test]
fn looks_like_oss_secret_value_heuristic() {
    assert!(looks_like_oss_secret_value(
        r#"{"endpoint":"oss-cn-guangzhou.aliyuncs.com","bucket":"b","access_key_id":"a","access_key_secret":"s"}"#
    ));
    assert!(!looks_like_oss_secret_value(""));
    assert!(!looks_like_oss_secret_value("plain-text"));
    assert!(!looks_like_oss_secret_value(
        r#"{"endpoint":"e","bucket":"b"}"#
    ));
    assert!(!looks_like_oss_secret_value(
        r#"{"username":"u","password":"p"}"#
    ));
}

#[test]
fn sign_url_shape_and_idempotence() {
    let auth = sample_auth();
    let key = "config_tpl/demo-app/v1.4.0/configs.tar.gz";

    let url = sign_oss_get_url(&auth, key, 3600).expect("sign should succeed");

    // Host = bucket.endpoint, path = object key
    assert!(
        url.starts_with("https://example-bucket.oss-cn-guangzhou.aliyuncs.com/config_tpl/demo-app/v1.4.0/configs.tar.gz?"),
        "unexpected URL: {url}"
    );
    assert!(url.contains("OSSAccessKeyId=LTAI_TEST_KEY_ID"), "{url}");
    assert!(url.contains("Expires="), "{url}");
    assert!(url.contains("Signature="), "{url}");

    // Expires is a unix timestamp and within the requested TTL window.
    let expires: i64 = url
        .split("Expires=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .and_then(|s| s.parse().ok())
        .expect("Expires should parse");
    let now = chrono::Utc::now().timestamp();
    assert!(expires > now, "Expires must be in the future");
    assert!(expires <= now + 3600, "Expires within TTL window");

    // Same input → same output (deterministic signature).
    let again = sign_oss_get_url(&auth, key, 3600).expect("sign again");
    assert_eq!(url, again);
}

#[test]
fn sign_ignores_leading_slash_in_key() {
    let auth = sample_auth();
    let a = sign_oss_get_url(&auth, "/config_tpl/x/configs.tar.gz", 3600).unwrap();
    let b = sign_oss_get_url(&auth, "config_tpl/x/configs.tar.gz", 3600).unwrap();
    assert_eq!(a, b);
}

#[test]
fn sign_rejects_bad_inputs() {
    let auth = sample_auth();
    assert!(
        sign_oss_get_url(&auth, "", 3600).is_err(),
        "empty object key rejected"
    );
    assert!(
        sign_oss_get_url(&auth, "  ", 3600).is_err(),
        "whitespace object key rejected"
    );

    let mut no_ak = sample_auth();
    no_ak.access_key_id = String::new();
    assert!(sign_oss_get_url(&no_ak, "a/b.tar.gz", 3600).is_err());

    let mut no_sk = sample_auth();
    no_sk.access_key_secret = String::new();
    assert!(sign_oss_get_url(&no_sk, "a/b.tar.gz", 3600).is_err());

    let mut no_bucket = sample_auth();
    no_bucket.bucket = String::new();
    assert!(sign_oss_get_url(&no_bucket, "a/b.tar.gz", 3600).is_err());
}

#[test]
fn sign_floors_ttl_at_one_minute() {
    let auth = sample_auth();
    let url = sign_oss_get_url(&auth, "a.tar.gz", 1).expect("ttl floored to 60s");
    let expires: i64 = url
        .split("Expires=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .and_then(|s| s.parse().ok())
        .expect("Expires should parse");
    let now = chrono::Utc::now().timestamp();
    assert!(
        expires >= now + 60,
        "min TTL 60s, got {expires} vs now {now}"
    );
}

#[test]
fn sign_percent_encodes_spaces_in_key() {
    let auth = sample_auth();
    let url = sign_oss_get_url(&auth, "dir name/cfg.tar.gz", 3600).unwrap();
    assert!(url.contains("dir%20name/cfg.tar.gz"), "{url}");
}

#[test]
fn sign_meta_reports_the_signed_window_matching_the_url() {
    let auth = sample_auth();
    let signed = sign_oss_get_url_meta(&auth, "config_tpl/x/configs.tar.gz", 3600)
        .expect("sign should succeed");

    // The reported window must match the `Expires` actually embedded in the URL.
    let expires_in_url: i64 = signed
        .url
        .split("Expires=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .and_then(|s| s.parse().ok())
        .expect("Expires should parse");
    assert_eq!(expires_in_url, signed.expires_epoch);
    assert_eq!(signed.ttl_secs(), 3600);

    // Signed-at is the Manager clock reading at signing: now, give or take a second.
    let now = chrono::Utc::now().timestamp();
    assert!(
        (now - signed.signed_at_epoch).abs() <= 1,
        "signed_at {} should be ~now {now}",
        signed.signed_at_epoch
    );

    // Older callers keep getting the bare URL (same shape, no metadata).
    let bare = sign_oss_get_url(&auth, "config_tpl/x/configs.tar.gz", 3600).unwrap();
    assert!(bare.starts_with("https://example-bucket.oss-cn-guangzhou.aliyuncs.com/"));
    assert!(bare.contains("Signature="));
}

#[test]
fn sign_meta_ttl_is_floored_like_the_url() {
    let auth = sample_auth();
    let signed = sign_oss_get_url_meta(&auth, "a.tar.gz", 5).expect("sign");
    assert_eq!(signed.ttl_secs(), 60, "TTL floor applies to the window too");
    assert_eq!(signed.expires_epoch - signed.signed_at_epoch, 60);
}

#[test]
fn sign_meta_propagates_input_errors() {
    let auth = sample_auth();
    let err = sign_oss_get_url_meta(&auth, "  ", 3600).expect_err("empty key rejected");
    assert!(err.contains("object key is empty"), "{err}");
}
