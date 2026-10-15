//! OpenAPI document for the Manager REST API.
//!
//! Foundation for the API-spec initiative: endpoints join the document by
//! annotating their handlers with `#[utoipa::path]` and listing them in
//! [`ApiDoc`]'s `paths(...)`. The spec is served unauthenticated at
//! `/api/v1/openapi.json` and grows incrementally as handlers are touched —
//! it is never hand-edited.

use axum::Json;
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(title = "Buz Manager API", description = "Buz Manager REST API"),
    paths(crate::api::version::health)
)]
pub struct ApiDoc;

/// Serve the generated OpenAPI document (no auth — it describes the API,
/// it grants nothing).
pub async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_contains_the_annotated_health_path() {
        let doc = ApiDoc::openapi();
        let json = serde_json::to_value(&doc).expect("spec serializes");
        assert_eq!(json["openapi"], "3.1.0");
        assert!(
            json["paths"]["/health"]["get"].is_object(),
            "health endpoint must be part of the generated spec: {json:?}"
        );
    }
}
