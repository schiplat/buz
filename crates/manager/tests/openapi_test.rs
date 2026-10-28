use manager::api::openapi::*;
use utoipa::OpenApi;

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
