//! Protocol round-trips for secret create/update (`team_shared`).

use shared::protocol::{CreateSecretRequest, UpdateSecretRequest};

#[test]
fn create_secret_team_shared_defaults_to_none() {
    let req: CreateSecretRequest = serde_json::from_str(
        r#"{"name":"X","value":"secret","team_id":"t1"}"#,
    )
    .expect("parse");
    assert_eq!(req.team_shared, None);
}

#[test]
fn create_secret_team_shared_true() {
    let req: CreateSecretRequest = serde_json::from_str(
        r#"{"name":"X","value":"secret","team_id":"t1","team_shared":true}"#,
    )
    .expect("parse");
    assert_eq!(req.team_shared, Some(true));
}

#[test]
fn update_secret_team_shared_optional() {
    let req: UpdateSecretRequest = serde_json::from_str(
        r#"{"value":"","team_shared":false}"#,
    )
    .expect("parse");
    assert_eq!(req.team_shared, Some(false));
    assert!(req.value.is_empty());
}
