use shared::protocol::{LogChunk, LogLevel, UpdateRunnerRequest};

#[test]
fn log_chunk_level_defaults_to_none_for_legacy_payloads() {
    let legacy: LogChunk =
        serde_json::from_str(r#"{"chunk_index":1,"step_order":0,"content":"x"}"#).unwrap();
    assert_eq!(legacy.level, None);

    let stamped: LogChunk =
        serde_json::from_str(r#"{"chunk_index":1,"step_order":0,"content":"x","level":"error"}"#)
            .unwrap();
    assert_eq!(stamped.level, Some(LogLevel::Error));

    // Round-trip keeps the lowercase wire form and rejects junk levels.
    let wire = serde_json::to_string(&stamped).unwrap();
    assert!(wire.contains(r#""level":"error""#), "{wire}");
    let bad = serde_json::from_str::<LogChunk>(
        r#"{"chunk_index":1,"step_order":0,"content":"x","level":"fatal"}"#,
    );
    assert!(bad.is_err());
}

#[test]
fn runner_team_patch_distinguishes_omitted_null_and_value() {
    let omitted: UpdateRunnerRequest = serde_json::from_str("{}").unwrap();
    assert_eq!(omitted.team_id, None);

    let shared: UpdateRunnerRequest = serde_json::from_str(r#"{"team_id":null}"#).unwrap();
    assert_eq!(shared.team_id, Some(None));

    let bound: UpdateRunnerRequest = serde_json::from_str(r#"{"team_id":"team-1"}"#).unwrap();
    assert_eq!(bound.team_id, Some(Some("team-1".to_string())));
}
