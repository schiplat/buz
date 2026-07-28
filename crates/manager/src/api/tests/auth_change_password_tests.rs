use super::validate_change_password;
use shared::protocol::ChangePasswordRequest;

#[test]
fn rejects_empty_current() {
    let err = validate_change_password(&ChangePasswordRequest {
        current_password: "".into(),
        new_password: "abcdef".into(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("current_password"));
}

#[test]
fn rejects_empty_new() {
    let err = validate_change_password(&ChangePasswordRequest {
        current_password: "oldpass".into(),
        new_password: "".into(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("new_password"));
}

#[test]
fn rejects_short_new() {
    let err = validate_change_password(&ChangePasswordRequest {
        current_password: "oldpass".into(),
        new_password: "abc".into(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("at least 6"));
}

#[test]
fn rejects_same_as_current() {
    let err = validate_change_password(&ChangePasswordRequest {
        current_password: "samepass".into(),
        new_password: "samepass".into(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("differ"));
}

#[test]
fn accepts_valid_pair() {
    assert!(validate_change_password(&ChangePasswordRequest {
        current_password: "oldpass".into(),
        new_password: "newpass1".into(),
    })
    .is_ok());
}
