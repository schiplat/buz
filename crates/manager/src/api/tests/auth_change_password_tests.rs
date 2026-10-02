use super::{validate_change_password, validate_setup};
use shared::protocol::{ChangePasswordRequest, SetupAdminRequest};

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
        current_password: "Oldpass1!".into(),
        new_password: "abc".into(),
    })
    .unwrap_err();
    // Same complexity policy as setup (>= 8 chars + character classes).
    assert!(err.to_string().contains("at least 8"));
}

#[test]
fn rejects_same_as_current() {
    let err = validate_change_password(&ChangePasswordRequest {
        current_password: "Samepass1!".into(),
        new_password: "Samepass1!".into(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("differ"));
}

#[test]
fn accepts_valid_pair() {
    assert!(validate_change_password(&ChangePasswordRequest {
        current_password: "Oldpass1!".into(),
        new_password: "Newpass2!".into(),
    })
    .is_ok());
}

#[test]
fn setup_rejects_short_password() {
    let err = validate_setup(&SetupAdminRequest {
        username: "admin".into(),
        password: "short".into(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("at least 8"));
}

#[test]
fn setup_rejects_weak_password() {
    let err = validate_setup(&SetupAdminRequest {
        username: "admin".into(),
        password: "alllowercase1!".into(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("uppercase") || err.to_string().contains("symbol"));
}

#[test]
fn setup_rejects_bad_username() {
    assert!(validate_setup(&SetupAdminRequest {
        username: "a".into(),
        password: "Str0ng!Pass".into(),
    })
    .is_err());
    assert!(validate_setup(&SetupAdminRequest {
        username: "bad name!".into(),
        password: "Str0ng!Pass".into(),
    })
    .is_err());
}

#[test]
fn setup_accepts_valid() {
    assert!(validate_setup(&SetupAdminRequest {
        username: "admin".into(),
        password: "Str0ng!Pass".into(),
    })
    .is_ok());
}
