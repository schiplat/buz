use axum::Json;
use shared::version::{PRODUCT_NAME, VERSION};

/// Public health + version probe (no auth).
pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "name": PRODUCT_NAME,
        "version": VERSION,
        "component": "manager",
    }))
}

#[cfg(test)]
#[path = "tests/version_tests.rs"]
mod tests;
