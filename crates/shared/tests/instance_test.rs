//! Tests for `shared::instance` — identity validation, env resolution, and
//! persistence semantics (env > state file > generated).

use shared::instance::{self, IdSource, ID_MAX_LEN};
use uuid::Uuid;

#[test]
fn validate_accepts_plain_ids() {
    for ok in ["a", "mgr-01", "Host.1_x", "sandboxd-hostA-01"] {
        assert_eq!(instance::validate(ok).unwrap(), ok);
    }
}

#[test]
fn validate_rejects_bad_ids() {
    for bad in [
        "",
        " ",
        "-lead",
        ".lead",
        "_lead",
        "has space",
        "has/slash",
        "tab\tid",
        "ünicode",
    ] {
        assert!(instance::validate(bad).is_err(), "expected reject: {bad:?}");
    }
    let long = "a".repeat(ID_MAX_LEN + 1);
    assert!(instance::validate(&long).is_err());
    assert_eq!(
        instance::validate(&"a".repeat(ID_MAX_LEN)).unwrap().len(),
        ID_MAX_LEN
    );
}

#[test]
fn from_env_reads_validates_and_treats_empty_as_unset() {
    let var = "BUZ_TEST_ID_FROM_ENV";
    std::env::remove_var(var);
    assert_eq!(instance::from_env(var).unwrap(), None);
    std::env::set_var(var, " host-7 ");
    assert_eq!(instance::from_env(var).unwrap(), Some("host-7".into()));
    std::env::set_var(var, "");
    assert_eq!(instance::from_env(var).unwrap(), None);
    std::env::set_var(var, "bad id");
    assert!(instance::from_env(var).is_err());
    std::env::remove_var(var);
}

#[test]
fn resolve_host_id_prefers_env() {
    let var = "BUZ_TEST_HOST_ID";
    std::env::set_var(var, "env-host-1");
    let (id, source) = instance::resolve_host_id(var).unwrap();
    assert_eq!(id, "env-host-1");
    assert_eq!(source, IdSource::Env);
    std::env::remove_var(var);
}

#[test]
fn resolve_host_id_falls_back_to_a_valid_id() {
    let var = "BUZ_TEST_HOST_ID_UNSET";
    std::env::remove_var(var);
    let (id, source) = match instance::resolve_host_id(var) {
        Ok(resolved) => resolved,
        Err(_) => return, // no machine-id/hostname on this host — fine
    };
    instance::validate(&id).unwrap();
    assert!(matches!(source, IdSource::MachineId | IdSource::Hostname));
}

#[test]
fn ensure_persistent_generates_once_then_reuses() {
    let dir = std::env::temp_dir().join(format!("buz-instance-{}", Uuid::new_v4()));
    let var = "BUZ_TEST_PERSIST_ID";
    std::env::remove_var(var);
    let (first, source) = instance::ensure_persistent(&dir, "inst-id", var, "rn").unwrap();
    assert_eq!(source, IdSource::Generated);
    instance::validate(&first).unwrap();
    let (second, source) = instance::ensure_persistent(&dir, "inst-id", var, "rn").unwrap();
    assert_eq!(source, IdSource::StateFile);
    assert_eq!(first, second);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ensure_persistent_env_overrides_state_file() {
    let dir = std::env::temp_dir().join(format!("buz-instance-{}", Uuid::new_v4()));
    let var = "BUZ_TEST_PERSIST_ID_ENV";
    std::env::remove_var(var);
    let (generated, _) = instance::ensure_persistent(&dir, "inst-id", var, "rn").unwrap();
    std::env::set_var(var, "pinned-9");
    let (pinned, source) = instance::ensure_persistent(&dir, "inst-id", var, "rn").unwrap();
    assert_eq!(source, IdSource::Env);
    assert_eq!(pinned, "pinned-9");
    assert_ne!(pinned, generated);
    std::env::remove_var(var);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ensure_persistent_rejects_invalid_state_file_contents() {
    let dir = std::env::temp_dir().join(format!("buz-instance-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let var = "BUZ_TEST_PERSIST_ID_BAD";
    std::env::remove_var(var);
    std::fs::write(dir.join("inst-id"), "not a valid id!\n").unwrap();
    assert!(instance::ensure_persistent(&dir, "inst-id", var, "rn").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
