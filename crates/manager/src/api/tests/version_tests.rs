use super::health;
use axum::Json;
use shared::version::VERSION;

#[tokio::test]
async fn health_includes_version() {
    let Json(body) = health().await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], VERSION);
    assert_eq!(body["name"], "Buz");
    assert_eq!(body["component"], "manager");
}
