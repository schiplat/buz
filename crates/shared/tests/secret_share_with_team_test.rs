//! Protocol round-trips for secret create/update (`share_with_team`).

use shared::protocol::{CreateSecretRequest, UpdateSecretRequest};

#[test]
fn create_secret_share_with_team_defaults_to_none() {
    let req: CreateSecretRequest =
        serde_json::from_str(r#"{"name":"X","value":"secret","team_id":"t1"}"#).expect("parse");
    assert_eq!(req.share_with_team, None);
}

#[test]
fn create_secret_share_with_team_true() {
    let req: CreateSecretRequest = serde_json::from_str(
        r#"{"name":"X","value":"secret","team_id":"t1","share_with_team":true}"#,
    )
    .expect("parse");
    assert_eq!(req.share_with_team, Some(true));
}

#[test]
fn update_secret_share_with_team_optional() {
    let req: UpdateSecretRequest =
        serde_json::from_str(r#"{"value":"","share_with_team":false}"#).expect("parse");
    assert_eq!(req.share_with_team, Some(false));
    assert!(req.value.is_empty());
}

#[test]
fn create_secret_kind_optional() {
    let req: CreateSecretRequest =
        serde_json::from_str(r#"{"name":"X","value":"secret","team_id":"t1","kind":"registry"}"#)
            .expect("parse");
    assert_eq!(req.kind.as_deref(), Some("registry"));
}
