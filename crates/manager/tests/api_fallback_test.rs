//! Unknown `/api/v1/*` paths answer a JSON 404.
//!
//! The full app router puts the API under `/api/v1` and keeps the SPA
//! fallback on the outer router. Without a fallback inside the API router, an
//! unmatched API path falls through to the SPA and returns `index.html` with
//! **200** — a new dashboard calling a route an older Manager lacks would then
//! parse HTML as a response instead of failing loudly.

mod common;

use axum::http::StatusCode;
use common::TestApp;

#[tokio::test]
async fn unknown_api_paths_answer_json_404() {
    let app = TestApp::boot().await;

    let (status, body) = app.get("/api/v1/definitely-not-a-route", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], 2001, "the API's own NotFound code");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no such API endpoint"),
        "unexpected body: {body}"
    );
    assert!(body.get("data").is_some(), "envelope shape is preserved");
}

#[tokio::test]
async fn real_routes_still_match() {
    let app = TestApp::boot().await;

    // A public read that exists: the fallback must not shadow it.
    let (status, body) = app.get("/api/v1/branding", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], 0);
}
