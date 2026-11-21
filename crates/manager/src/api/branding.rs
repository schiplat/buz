//! Instance branding: an administrator-uploaded logo (docs `platform/branding.md`).
//!
//! **The built-in mark is the default.** An absent row means the SPA keeps its
//! bundled "Bz" logo; this module only ever adds a replacement. Reads are
//! public because the sign-in page needs the logo before authentication — a
//! logo is branding, not a secret. Writes are platform-admin only.
//!
//! The image is stored as base64 text (one statement body for SQLite and
//! Postgres). Uploads are validated three times: size before decoding, base64,
//! then the declared MIME against the actual bytes. SVG is accepted because
//! logos are vector art, but only after a structural reject-list — and the read
//! endpoint always answers with `nosniff` plus a restrictive CSP, so a crafted
//! file cannot execute even when opened directly.

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;

use crate::audit::{self, Action, AuditEvent, Resource};
use crate::middleware::PlatformAdmin;
use crate::state::AppState;

/// Decoded-size cap for the uploaded file.
pub const MAX_LOGO_BYTES: usize = 512 * 1024;
/// Base64 expands by 4/3; reject before decoding so a huge body never allocates.
const MAX_BASE64_LEN: usize = MAX_LOGO_BYTES / 3 * 4 + 8;

/// `GET /api/v1/branding` — public branding descriptor.
pub async fn get_branding(
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let row = crate::store::branding::get(&state.pool).await?;
    Ok(Json(ApiResponse::success(payload(row.as_ref()))))
}

/// `GET /api/v1/branding/logo` — the uploaded image, or 404 when none.
pub async fn get_logo(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(row) = crate::store::branding::get(&state.pool).await? else {
        return Err(AppError::NotFound("no custom logo configured".into()));
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(row.logo_base64.trim())
        .map_err(|e| AppError::Internal(format!("stored logo unreadable: {e}")))?;
    let etag = format!("\"{}\"", etag_hex(&bytes));

    // Revalidation: ETag is content-derived, so a 304 is always correct.
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        == Some(etag.as_str())
    {
        let mut not_modified = StatusCode::NOT_MODIFIED.into_response();
        if let Ok(value) = HeaderValue::from_str(&etag) {
            not_modified.headers_mut().insert(header::ETAG, value);
        }
        return Ok(not_modified);
    }

    let mut response = Response::new(Body::from(bytes));
    let out = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&row.logo_mime) {
        out.insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&etag) {
        out.insert(header::ETAG, value);
    }
    out.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    out.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    out.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // Defence in depth for SVG: even if the file were opened directly, no
    // script or external resource may run (the upload check already rejects
    // both, this covers anything the check misses).
    out.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; img-src data:"),
    );
    Ok(response)
}

#[derive(Deserialize)]
pub struct UpdateBrandingRequest {
    /// Declared MIME (a `data:` URL prefix in `data_base64` is tolerated).
    pub content_type: String,
    pub data_base64: String,
}

/// `PUT /api/v1/admin/branding` — validate, store, audit.
pub async fn update_branding(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<UpdateBrandingRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let data = strip_data_url_prefix(req.data_base64.trim());
    if data.is_empty() {
        return Err(AppError::ValidationFailed("logo data is required".into()));
    }
    // Length-first: a body that could not possibly fit is rejected before
    // base64 decoding allocates anything.
    if data.len() > MAX_BASE64_LEN {
        return Err(too_large());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| AppError::ValidationFailed("logo data is not valid base64".into()))?;
    if bytes.is_empty() {
        return Err(AppError::ValidationFailed("logo data is empty".into()));
    }
    if bytes.len() > MAX_LOGO_BYTES {
        return Err(too_large());
    }
    let mime = validate_logo(&req.content_type, &bytes).map_err(AppError::ValidationFailed)?;

    crate::store::branding::upsert(
        &state.pool,
        mime,
        &base64::engine::general_purpose::STANDARD.encode(&bytes),
        bytes.len() as i64,
        &auth.username,
    )
    .await?;

    // Metadata only — the image bytes never reach the audit log.
    audit::record(
        &state.pool,
        AuditEvent::new(Action::BrandingUpdate, &auth)
            .on(Resource::Setting {
                key: "branding.logo",
            })
            .after(Some(json!({ "mime": mime, "size_bytes": bytes.len() }))),
    )
    .await;

    let row = crate::store::branding::get(&state.pool).await?;
    Ok(Json(ApiResponse::success(payload(row.as_ref()))))
}

/// `DELETE /api/v1/admin/branding` — drop the upload; the built-in mark returns.
pub async fn reset_branding(
    auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let existed = crate::store::branding::delete(&state.pool).await?;
    if existed {
        audit::record(
            &state.pool,
            AuditEvent::new(Action::BrandingReset, &auth).on(Resource::Setting {
                key: "branding.logo",
            }),
        )
        .await;
    }
    Ok(Json(ApiResponse::success(payload(None))))
}

fn too_large() -> AppError {
    AppError::ValidationFailed(format!(
        "logo is too large — the maximum is {} KB",
        MAX_LOGO_BYTES / 1024
    ))
}

fn etag_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Public descriptor: what the UI needs to render and what the upload form
/// documents. `recommendations` lives here so the copy shown to admins and the
/// limits the server enforces can never drift apart.
fn payload(row: Option<&crate::store::branding::BrandingRow>) -> Value {
    let custom = row.is_some();
    let etag = row.and_then(|row| {
        base64::engine::general_purpose::STANDARD
            .decode(row.logo_base64.trim())
            .ok()
            .map(|bytes| etag_hex(&bytes))
    });
    json!({
        "custom": custom,
        "logo_url": "/api/v1/branding/logo",
        "mime": row.map(|r| r.logo_mime.clone()),
        "size_bytes": row.map(|r| r.logo_bytes),
        "updated_by": row.map(|r| r.updated_by.clone()),
        "updated_at": row.map(|r| r.updated_at.clone()),
        // Content-derived version for `?v=` cache busting; absent when built-in.
        "etag": etag,
        "recommendations": {
            "formats": ["SVG", "PNG", "WebP", "JPEG"],
            "preferred_format": "SVG or PNG",
            "dimensions": "512 × 512 px (square)",
            "min_dimensions": "128 × 128 px",
            "max_bytes": MAX_LOGO_BYTES,
            "transparent_background": true,
            "safe_margin": "at least 10% on every side",
            "note": "Square icon mark — shown at 32 px in the sidebar and 40 px on sign-in pages. A horizontal wordmark is not supported."
        }
    })
}

fn strip_data_url_prefix(data: &str) -> &str {
    match data.split_once(',') {
        Some((head, rest)) if head.starts_with("data:") => rest,
        _ => data,
    }
}

/// Normalise the declared MIME and verify the bytes really are that format.
fn validate_logo(content_type: &str, bytes: &[u8]) -> Result<&'static str, String> {
    let declared = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mime = match declared.as_str() {
        "image/png" => "image/png",
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/webp" => "image/webp",
        "image/svg+xml" => "image/svg+xml",
        other => {
            return Err(format!(
                "unsupported image type {other:?} — use SVG, PNG, WebP or JPEG"
            ))
        }
    };
    let matches = match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/webp" => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "image/svg+xml" => {
            check_svg(bytes)?;
            true
        }
        _ => unreachable!("allowlist is exhaustive"),
    };
    if !matches {
        return Err(format!(
            "logo content does not match the declared type {mime:?}"
        ));
    }
    Ok(mime)
}

/// Structural reject-list for SVG uploads. Any match is refused outright —
/// there is no sanitising pass, so an unusual-but-clean file must be re-exported
/// by the administrator. `<img>` rendering already isolates scripts; the CSP on
/// the read endpoint covers direct navigation.
fn check_svg(bytes: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "SVG must be UTF-8 text".to_string())?;
    let lower = text.to_ascii_lowercase();
    let body = lower
        .trim_start_matches('\u{feff}')
        .trim_start()
        .trim_start_matches("<?xml");
    // After an optional XML declaration the root element must be <svg>.
    let body = if body.len() != lower.trim_start().len() {
        match body.find("?>") {
            Some(end) => body[end + 2..].trim_start(),
            None => body,
        }
    } else {
        body
    };
    if !body.starts_with("<svg") {
        return Err("SVG root element must be <svg>".into());
    }
    for needle in [
        "<!doctype",
        "<!entity",
        "<script",
        "<foreignobject",
        "javascript:",
        "vbscript:",
    ] {
        if lower.contains(needle) {
            return Err(format!("SVG rejected: it contains {needle:?}"));
        }
    }
    if has_event_handler(&lower) {
        return Err("SVG rejected: it contains an inline event handler".into());
    }
    if has_external_reference(&lower) {
        return Err("SVG rejected: it references an external resource".into());
    }
    Ok(())
}

/// `on<letters> =` used as an attribute (not the same substring inside a word).
fn has_event_handler(lower: &str) -> bool {
    let bytes = lower.as_bytes();
    let mut cursor = 0;
    while let Some(offset) = lower[cursor..].find("on") {
        let start = cursor + offset;
        let mut end = start + 2;
        while end < bytes.len() && bytes[end].is_ascii_alphabetic() {
            end += 1;
        }
        if end > start + 2 {
            let mut eq = end;
            while eq < bytes.len() && bytes[eq].is_ascii_whitespace() {
                eq += 1;
            }
            let preceding_is_boundary = start == 0
                || bytes[start - 1].is_ascii_whitespace()
                || bytes[start - 1] == b'<'
                || bytes[start - 1] == b'/';
            if preceding_is_boundary && eq < bytes.len() && bytes[eq] == b'=' {
                return true;
            }
        }
        cursor = start + 2;
    }
    false
}

/// Reject `href` values that leave the document, plus CSS `url(http…)`.
/// In-document references (`href="#id"`) and `data:image/…` stay allowed.
fn has_external_reference(lower: &str) -> bool {
    if lower.contains("url(http") || lower.contains("url('http") || lower.contains("url(\"http") {
        return true;
    }
    let mut cursor = 0;
    while let Some(offset) = lower[cursor..].find("href") {
        let start = cursor + offset;
        let Some(eq) = lower[start..].find('=').map(|i| start + i) else {
            break;
        };
        let rest = lower[eq + 1..].trim_start();
        let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            cursor = start + 4;
            continue;
        };
        let value_start = eq + 1 + (lower[eq + 1..].len() - rest.len()) + 1;
        let value_end = lower[value_start..]
            .find(quote)
            .map(|i| value_start + i)
            .unwrap_or(lower.len());
        let value = &lower[value_start..value_end];
        if value.contains("://")
            || value.starts_with("//")
            || (value.starts_with("data:") && !value.starts_with("data:image/"))
        {
            return true;
        }
        cursor = value_end;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

    #[test]
    fn mime_must_match_magic_bytes() {
        assert_eq!(validate_logo("image/png", PNG).unwrap(), "image/png");
        assert!(validate_logo("image/jpeg", PNG).is_err());
        assert!(validate_logo("image/gif", PNG).is_err());
    }

    #[test]
    fn svg_accepts_plain_art_and_rejects_active_content() {
        let clean = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h10v10z"/></svg>"#;
        assert_eq!(
            validate_logo("image/svg+xml", clean).unwrap(),
            "image/svg+xml"
        );

        for bad in [
            br#"<svg><script>alert(1)</script></svg>"#.as_slice(),
            br#"<svg onload="alert(1)"></svg>"#,
            br#"<svg><foreignObject/></svg>"#,
            br#"<!DOCTYPE svg><svg></svg>"#,
            br#"<svg><image href="https://evil.example/x.png"/></svg>"#,
            br#"<svg><style>@import url(http://evil.example/x);</style></svg>"#,
            b"not an svg",
        ] {
            assert!(
                validate_logo("image/svg+xml", bad).is_err(),
                "expected rejection for {:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn event_handler_detection_ignores_words() {
        assert!(has_event_handler("<svg onclick = \"x\">"));
        assert!(!has_event_handler("<svg><button>text</button></svg>"));
        assert!(!has_event_handler("<svg><path d=\"M0 0\"/></svg>"));
    }

    #[test]
    fn data_url_prefix_is_stripped() {
        assert_eq!(strip_data_url_prefix("data:image/png;base64,AAA"), "AAA");
        assert_eq!(strip_data_url_prefix("AAA"), "AAA");
    }
}
