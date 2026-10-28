use manager::api::runners::parse_execution_mode;
use shared::error::AppError;
use shared::protocol::RunnerExecutionMode;

#[test]
fn every_tier_name_parses() {
    for mode in RunnerExecutionMode::ALL {
        assert_eq!(
            parse_execution_mode(mode.as_str().to_string()).unwrap(),
            mode
        );
    }
}

#[test]
fn unknown_tier_names_are_rejected_with_the_accepted_ladder() {
    for value in ["gvisor", "firecracker", "kata", "", "Container"] {
        let error = parse_execution_mode(value.to_string()).unwrap_err();
        assert!(matches!(error, AppError::ValidationFailed(_)), "{error}");
    }
    let error = parse_execution_mode("nope".to_string()).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("host | container | sandbox | vm"),
        "{message}"
    );
}
