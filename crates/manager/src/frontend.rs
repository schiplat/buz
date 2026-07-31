//! Serve the Vue SPA from assets embedded in the binary (rust-embed).
//! Optional disk override via `--frontend` / `BUZ_FRONTEND_DIR` for local UI work.

use axum::{
    body::Body,
    http::{header, HeaderValue, Response, StatusCode, Uri},
    response::IntoResponse,
};
use rust_embed::Embed;
use tower_http::services::{ServeDir, ServeFile};

#[derive(Embed)]
#[folder = "../../frontend/dist"]
#[prefix = ""]
struct EmbeddedAssets;

/// SPA fallback: try exact path, then `index.html`.
pub async fn embedded_handler(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match EmbeddedAssets::get(path) {
        Some(file) => asset_response(path, file.data.as_ref()),
        None => match EmbeddedAssets::get("index.html") {
            Some(file) => asset_response("index.html", file.data.as_ref()),
            None => (StatusCode::NOT_FOUND, "frontend not embedded").into_response(),
        },
    }
}

fn asset_response(path: &str, data: &[u8]) -> Response<Body> {
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    let mut res = Response::new(Body::from(data.to_vec()));
    *res.status_mut() = StatusCode::OK;
    if let Ok(v) = HeaderValue::from_str(&mime) {
        res.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    // Fingerprinted Vite assets under /assets/ are immutable; HTML should revalidate.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    if let Ok(v) = HeaderValue::from_str(cache) {
        res.headers_mut().insert(header::CACHE_CONTROL, v);
    }
    res
}

/// Disk-based SPA (dev override) for `Router::fallback_service`.
pub fn disk_fallback(frontend_dir: &str) -> ServeDir<ServeFile> {
    let index_html = format!("{frontend_dir}/index.html");
    ServeDir::new(frontend_dir).fallback(ServeFile::new(index_html))
}

pub fn embedded_file_count() -> usize {
    EmbeddedAssets::iter().count()
}
