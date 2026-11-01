use manager::audit::redact_value;
use manager::audit::secret_snapshot;
use serde_json::json;

#[test]
fn redact_nested_password_and_value() {
    let v = json!({
        "name": "acr",
        "value": "super-secret-password",
        "nested": { "password": "hunter2", "registry": "cr.example.com" }
    });
    let r = redact_value(&v);
    assert_eq!(r["name"], "acr");
    assert_ne!(r["value"], "super-secret-password");
    assert!(r["value"].as_str().unwrap().contains("****"));
    assert_ne!(r["nested"]["password"], "hunter2");
    assert_eq!(r["nested"]["registry"], "cr.example.com");
}

#[test]
fn secret_snapshot_never_has_raw_secret() {
    let s = secret_snapshot("id1", "S", "slug", "tid", false, "generic");
    assert_eq!(s["value"], "***");
    assert_eq!(s["name"], "S");
}

#[test]
fn redact_credential_data() {
    let v = json!({ "name": "h1", "credential_data": "BEGIN PRIVATE KEY" });
    let r = redact_value(&v);
    assert_ne!(r["credential_data"], "BEGIN PRIVATE KEY");
}

#[test]
fn changes_records_only_differing_keys() {
    use manager::audit::Action;
    use manager::audit::AuditEvent;
    use manager::audit::Resource;
    use manager::rbac::AuthUser;

    let auth = AuthUser {
        id: "u1".into(),
        username: "alice".into(),
        is_platform_admin: true,
        is_super_admin: false,
        is_ops: false,
        memberships: vec![],
    };
    let event = AuditEvent::new(Action::RunnerUpdate, &auth)
        .on(Resource::runner("r1", "runner-one"))
        .changes(
            Some(json!({ "labels": ["a"], "max_concurrent_jobs": 1, "removed": "x" })),
            Some(json!({ "labels": ["a"], "max_concurrent_jobs": 4, "added": "y" })),
        );

    let before = event.before.unwrap();
    let after = event.after.unwrap();
    // Unchanged key dropped entirely.
    assert!(before.get("labels").is_none() && after.get("labels").is_none());
    // Changed key keeps both sides.
    assert_eq!(before["max_concurrent_jobs"], 1);
    assert_eq!(after["max_concurrent_jobs"], 4);
    // Removed key reads as null on the after side, added key as null before.
    assert_eq!(before["removed"], "x");
    assert!(after["removed"].is_null());
    assert!(before["added"].is_null());
    assert_eq!(after["added"], "y");
    // Resource columns derive consistently.
    assert_eq!(event.resource_type, "runner");
    assert_eq!(event.resource_id, "r1");
    assert_eq!(event.resource_name, "runner-one");
}

#[test]
fn plugin_pool_resource_composes_the_reference() {
    use manager::audit::Action;
    use manager::audit::AuditEvent;
    use manager::audit::Resource;
    use manager::rbac::AuthUser;

    let auth = AuthUser {
        id: "u1".into(),
        username: "alice".into(),
        is_platform_admin: true,
        is_super_admin: false,
        is_ops: false,
        memberships: vec![],
    };
    let event = AuditEvent::new(Action::PluginPoolToggle, &auth).on(Resource::plugin_pool(
        "buz",
        "helm-upgrade",
        "1.0.0",
    ));
    assert_eq!(event.action, "plugin_pool.toggle");
    assert_eq!(event.resource_type, "plugin_pool");
    assert_eq!(event.resource_id, "buz/helm-upgrade@1.0.0");
    assert_eq!(event.resource_name, "buz/helm-upgrade@1.0.0");
}
