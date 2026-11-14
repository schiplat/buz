//! `[cluster] mode` × storage kind: the startup gate and the single-instance
//! lock.
//!
//! The invariants under test:
//! - `multi` + SQLite is refused — SQLite has no cross-instance coordination,
//!   so two Managers on one file would corrupt scheduling state;
//! - a Postgres url is served when the binary carries the `postgres` feature
//!   and fails closed when it does not;
//! - `single` takes an exclusive `<db>.lock` flock for its lifetime: a second
//!   Manager on the same storage is refused, and the lock is released when the
//!   holder goes away.

use manager::config::{ClusterConfig, ClusterMode, DatabaseConfig, StorageKind};

fn database_with_url(url: Option<&str>) -> DatabaseConfig {
    DatabaseConfig {
        url: url.map(str::to_string),
        ..Default::default()
    }
}

// ── storage kind detection ──────────────────────────────────────────────

#[test]
fn legacy_path_means_sqlite() {
    let db = DatabaseConfig::default();
    assert_eq!(db.storage_kind(), Ok(StorageKind::Sqlite));
}

#[test]
fn url_schemes_are_detected() {
    assert_eq!(
        database_with_url(Some("sqlite:data/manager.db")).storage_kind(),
        Ok(StorageKind::Sqlite)
    );
    assert_eq!(
        database_with_url(Some("postgres://user@host/buz")).storage_kind(),
        Ok(StorageKind::Postgres)
    );
    assert_eq!(
        database_with_url(Some("postgresql://user@host/buz")).storage_kind(),
        Ok(StorageKind::Postgres)
    );
}

#[test]
fn unknown_scheme_is_an_error() {
    let err = database_with_url(Some("mysql://host/buz"))
        .storage_kind()
        .unwrap_err();
    assert!(err.contains("mysql"), "{err}");
}

#[test]
fn scheme_match_is_case_insensitive() {
    assert_eq!(
        database_with_url(Some("SQLITE:data/manager.db")).storage_kind(),
        Ok(StorageKind::Sqlite)
    );
}

// ── [cluster] config parsing ────────────────────────────────────────────

#[test]
fn cluster_mode_defaults_to_single() {
    assert_eq!(ClusterConfig::default().mode, ClusterMode::Single);
    let absent: ClusterConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(absent.mode, ClusterMode::Single);
}

#[test]
fn cluster_mode_parses_both_values() {
    let multi: ClusterConfig = serde_json::from_str(r#"{ "mode": "multi" }"#).unwrap();
    assert_eq!(multi.mode, ClusterMode::Multi);
    let single: ClusterConfig = serde_json::from_str(r#"{ "mode": "single" }"#).unwrap();
    assert_eq!(single.mode, ClusterMode::Single);
}

#[test]
fn cluster_mode_rejects_unknown_values() {
    assert!(serde_json::from_str::<ClusterConfig>(r#"{ "mode": "ha" }"#).is_err());
}

// ── startup gate ────────────────────────────────────────────────────────

#[test]
fn single_on_sqlite_is_accepted() {
    let db = DatabaseConfig::default();
    manager::db::validate_cluster_mode(ClusterMode::Single, &db).unwrap();
}

#[test]
fn single_on_sqlite_url_is_accepted() {
    let db = database_with_url(Some("sqlite:data/manager.db"));
    manager::db::validate_cluster_mode(ClusterMode::Single, &db).unwrap();
}

#[test]
fn multi_on_sqlite_is_refused_with_the_reason() {
    let db = DatabaseConfig::default();
    let err = manager::db::validate_cluster_mode(ClusterMode::Multi, &db).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("multi"), "{msg}");
    assert!(msg.contains("Postgres"), "must name the requirement: {msg}");
}

#[test]
fn postgres_url_is_gated_on_the_build_feature_only() {
    let db = database_with_url(Some("postgres://user@host/buz"));
    for mode in [ClusterMode::Single, ClusterMode::Multi] {
        let result = manager::db::validate_cluster_mode(mode, &db);
        #[cfg(feature = "postgres")]
        assert!(
            result.is_ok(),
            "Postgres serves both modes once the feature is built in: {result:?}"
        );
        #[cfg(not(feature = "postgres"))]
        {
            let err = result.expect_err("a binary without the feature must fail closed");
            assert!(err.to_string().to_lowercase().contains("postgres"), "{err}");
        }
    }
}

// ── single-instance lock ────────────────────────────────────────────────

#[test]
fn lock_acquires_then_blocks_then_releases() {
    let dir = tempfile::tempdir().unwrap();
    let db = DatabaseConfig {
        path: dir.path().join("manager.db").to_string_lossy().into_owned(),
        ..Default::default()
    };

    let first = manager::db::acquire_instance_lock(&db).expect("first acquire");
    assert!(
        manager::db::acquire_instance_lock(&db).is_err(),
        "a second Manager on the same storage must be refused"
    );

    drop(first);
    manager::db::acquire_instance_lock(&db)
        .expect("the lock must be released when the holder goes away");
}

#[test]
fn different_storages_do_not_contend() {
    let dir = tempfile::tempdir().unwrap();
    let a = DatabaseConfig {
        path: dir.path().join("a.db").to_string_lossy().into_owned(),
        ..Default::default()
    };
    let b = DatabaseConfig {
        path: dir.path().join("b.db").to_string_lossy().into_owned(),
        ..Default::default()
    };
    let _lock_a = manager::db::acquire_instance_lock(&a).expect("acquire a");
    let _lock_b = manager::db::acquire_instance_lock(&b).expect("acquire b");
}

#[test]
fn postgres_takes_no_file_lock() {
    let dir = tempfile::tempdir().unwrap();
    let db = DatabaseConfig {
        path: dir.path().join("unused.db").to_string_lossy().into_owned(),
        url: Some("postgres://user@host/buz".into()),
        ..Default::default()
    };
    manager::db::acquire_instance_lock(&db).expect("pg: no file lock to contend on");
}

// ── sqlite file resolution ──────────────────────────────────────────────

#[test]
fn url_wins_over_legacy_path() {
    let db = DatabaseConfig {
        path: "legacy/path.db".into(),
        url: Some("sqlite:new/path.db".into()),
        ..Default::default()
    };
    // The file resolution is private; the observable proxy is that the lock
    // sidecar lands next to the url's file.
    let _lock = manager::db::acquire_instance_lock(&db).unwrap();
    assert!(std::path::Path::new("new/path.db.lock").exists());
    assert!(!std::path::Path::new("legacy/path.db.lock").exists());
    std::fs::remove_file("new/path.db.lock").ok();
    std::fs::remove_dir_all("new").ok();
}
