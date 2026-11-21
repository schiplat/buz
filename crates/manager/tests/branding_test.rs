//! Instance branding API — default mark untouched, upload validation, cache
//! headers, reset, authorization and audit. Dual-backend via the shared
//! fixture (`BUZ_TEST_DB=pg` runs the same journeys on Postgres).

mod common;

use axum::http::{header, StatusCode};
use base64::Engine as _;
use common::{data, TestApp};
use serde_json::json;

/// A 1×1 transparent PNG (magic bytes `89 50 4E 47 0D 0A 1A 0A`).
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0xFC, 0xCF, 0xC0, 0x50,
    0x0F, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xA9, 0x8C, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn upload_body(content_type: &str, bytes: &[u8]) -> serde_json::Value {
    json!({ "content_type": content_type, "data_base64": b64(bytes) })
}

async fn logo_raw(app: &TestApp) -> (StatusCode, axum::http::HeaderMap, axum::body::Bytes) {
    app.call_raw("GET", "/api/v1/branding/logo", None, &[], None)
        .await
}

#[tokio::test]
async fn built_in_mark_is_the_default() {
    let app = TestApp::boot().await;

    let (status, body) = app.get("/api/v1/branding", None).await;
    assert_eq!(status, StatusCode::OK);
    let branding = data(&body);
    assert_eq!(branding["custom"], false);
    assert_eq!(branding["logo_url"], "/api/v1/branding/logo");
    assert!(branding["recommendations"]["max_bytes"].as_u64().unwrap() > 0);

    let (status, headers, bytes) = logo_raw(&app).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no upload → SPA keeps its built-in mark"
    );
    assert!(headers.get(header::CONTENT_TYPE).is_none() || !bytes.starts_with(b"\x89PNG"));
}

#[tokio::test]
async fn upload_serves_bytes_with_cache_headers() {
    let app = TestApp::boot().await;
    let token = app.admin_jwt();

    let (status, body) = app
        .put(
            "/api/v1/admin/branding",
            Some(&token),
            upload_body("image/png", PNG_1X1),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let branding = data(&body);
    assert_eq!(branding["custom"], true);
    assert_eq!(branding["mime"], "image/png");
    assert_eq!(branding["size_bytes"], PNG_1X1.len() as i64);
    assert_eq!(branding["updated_by"], "boss");
    let etag_hex = branding["etag"].as_str().expect("content etag").to_string();

    let (status, headers, bytes) = logo_raw(&app).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes.as_ref(), PNG_1X1);
    assert_eq!(headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(
        headers["content-security-policy"],
        "default-src 'none'; style-src 'unsafe-inline'; img-src data:"
    );
    assert_eq!(
        headers[header::ETAG],
        format!("\"{etag_hex}\""),
        "ETag is the content hash"
    );

    // Revalidation: same bytes → 304 with no body.
    let etag = headers[header::ETAG].to_str().unwrap().to_string();
    let (status, _headers, bytes) = app
        .call_raw(
            "GET",
            "/api/v1/branding/logo",
            None,
            &[("if-none-match", etag.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(bytes.is_empty());

    let count: i64 = manager::store::exec::fetch_optional(
        &app.pool,
        "SELECT COUNT(*) AS n FROM audit_logs WHERE action = 'branding.update'",
        &[],
    )
    .await
    .unwrap()
    .map(|row| row.i64_of("n"))
    .unwrap_or(0);
    assert_eq!(count, 1, "upload is audited");
}

#[tokio::test]
async fn upload_validation_rejects_bad_input() {
    let app = TestApp::boot().await;
    let token = app.admin_jwt();

    for (content_type, bytes) in [
        ("image/jpeg", PNG_1X1),          // declared type vs magic bytes
        ("image/gif", PNG_1X1),           // unsupported type
        ("image/png", b""),               // empty
        ("image/svg+xml", b"not an svg"), // not SVG at all
    ] {
        let (status, body) = app
            .put(
                "/api/v1/admin/branding",
                Some(&token),
                upload_body(content_type, bytes),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{content_type} / {} bytes should be rejected: {body}",
            bytes.len()
        );
    }

    // Invalid base64.
    let (status, _) = app
        .put(
            "/api/v1/admin/branding",
            Some(&token),
            json!({ "content_type": "image/png", "data_base64": "%%%" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Over the size cap — rejected before decoding.
    let huge = "A".repeat(699_200);
    let (status, _) = app
        .put(
            "/api/v1/admin/branding",
            Some(&token),
            json!({ "content_type": "image/png", "data_base64": huge }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Nothing was persisted by any rejected attempt.
    let (status, body) = app.get("/api/v1/branding", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data(&body)["custom"], false);
}

#[tokio::test]
async fn svg_upload_is_screened() {
    let app = TestApp::boot().await;
    let token = app.admin_jwt();

    let clean = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512"><rect width="512" height="512" fill="#1f9bf9"/></svg>"##;
    let (status, body) = app
        .put(
            "/api/v1/admin/branding",
            Some(&token),
            upload_body("image/svg+xml", clean),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(data(&body)["mime"], "image/svg+xml");

    for bad in [
        br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#.as_slice(),
        br#"<svg onload="alert(1)"></svg>"#,
        br#"<svg><foreignObject/></svg>"#,
        br#"<svg><image href="https://evil.example/x.png"/></svg>"#,
    ] {
        let (status, _) = app
            .put(
                "/api/v1/admin/branding",
                Some(&token),
                upload_body("image/svg+xml", bad),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "expected rejection for {:?}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[tokio::test]
async fn writes_require_a_platform_admin() {
    let app = TestApp::boot().await;
    let member = app.member_jwt();

    let (status, _) = app
        .put(
            "/api/v1/admin/branding",
            Some(&member),
            upload_body("image/png", PNG_1X1),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = app.delete("/api/v1/admin/branding", Some(&member)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Reads stay public — the sign-in page has no session yet.
    let (status, _) = app.get("/api/v1/branding", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn reset_restores_the_builtin_mark() {
    let app = TestApp::boot().await;
    let token = app.admin_jwt();

    let (status, _) = app
        .put(
            "/api/v1/admin/branding",
            Some(&token),
            upload_body("image/png", PNG_1X1),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = app.delete("/api/v1/admin/branding", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data(&body)["custom"], false);
    assert!(data(&body)["etag"].is_null());

    let (status, _, _) = logo_raw(&app).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let count: i64 = manager::store::exec::fetch_optional(
        &app.pool,
        "SELECT COUNT(*) AS n FROM audit_logs WHERE action = 'branding.reset'",
        &[],
    )
    .await
    .unwrap()
    .map(|row| row.i64_of("n"))
    .unwrap_or(0);
    assert_eq!(count, 1, "reset is audited");
}
