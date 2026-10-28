use runner::client::register::{cache_token, load_cached_token};
use runner::config::Config;
use std::path::Path;

fn config(root: &Path) -> Config {
    let mut config = Config::default();
    config.runner.workspace_dir = root.join("workspace").to_string_lossy().into_owned();
    config.runner.token_cache_path = root
        .join("state/runner-token")
        .to_string_lossy()
        .into_owned();
    config
}

#[test]
fn missing_new_cache_falls_back_to_legacy_cache() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let legacy = Path::new(&config.runner.workspace_dir).join("runner-token");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "legacy-jwt\n").unwrap();

    assert_eq!(
        load_cached_token(&config).unwrap().as_deref(),
        Some("legacy-jwt")
    );
}

#[test]
fn legacy_cache_migrates_after_new_cache_is_persisted() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let legacy = Path::new(&config.runner.workspace_dir).join("runner-token");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "legacy-jwt\n").unwrap();

    assert_eq!(
        load_cached_token(&config).unwrap().as_deref(),
        Some("legacy-jwt")
    );
    cache_token(&config, "legacy-jwt").unwrap();

    assert_eq!(
        load_cached_token(&config).unwrap().as_deref(),
        Some("legacy-jwt")
    );
    assert!(!legacy.exists());
}

#[test]
fn cache_write_replaces_existing_token_and_uses_private_permissions() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    cache_token(&config, "first-jwt").unwrap();
    cache_token(&config, "second-jwt").unwrap();

    assert_eq!(
        load_cached_token(&config).unwrap().as_deref(),
        Some("second-jwt")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(&config.runner.token_cache_path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }
}

#[cfg(unix)]
#[test]
fn insecure_new_cache_fails_without_falling_back_to_legacy() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let legacy = Path::new(&config.runner.workspace_dir).join("runner-token");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "legacy-jwt").unwrap();
    std::fs::create_dir_all(Path::new(&config.runner.token_cache_path).parent().unwrap()).unwrap();
    std::fs::write(&config.runner.token_cache_path, "insecure-jwt").unwrap();
    std::fs::set_permissions(
        &config.runner.token_cache_path,
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();

    let error = load_cached_token(&config).unwrap_err().to_string();
    assert!(error.contains("mode 0600"), "{error}");
    assert!(legacy.exists());
}

#[test]
fn non_regular_new_cache_fails_without_falling_back_to_legacy() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let legacy = Path::new(&config.runner.workspace_dir).join("runner-token");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "legacy-jwt").unwrap();
    std::fs::create_dir_all(Path::new(&config.runner.token_cache_path).parent().unwrap()).unwrap();
    std::fs::create_dir(Path::new(&config.runner.token_cache_path)).unwrap();

    let error = load_cached_token(&config).unwrap_err().to_string();
    assert!(error.contains("not a regular file"), "{error}");
    assert!(legacy.exists());
}

#[test]
fn empty_cache_is_an_error() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let path = Path::new(&config.runner.token_cache_path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "  \n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let error = load_cached_token(&config).unwrap_err().to_string();
    assert!(error.contains("empty"), "{error}");
}
