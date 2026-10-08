use crate::services::pipeline_scanner::{
    filename_from_pipeline_name, lock_yaml_file, normalize_yaml_path_key, read_yaml_for_scan,
    validate_yaml_filename, yaml_content_etag, yaml_path_storage,
};

#[tokio::test]
async fn scanner_read_waits_for_file_lock_and_observes_replaced_content() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scan_dir = dir.path().to_string_lossy().to_string();
    let filename = "scanner-lock.yml";
    let initial = "name: initial\nsteps: []\n";
    let replacement = "name: replacement\nsteps: []\n";
    std::fs::write(dir.path().join(filename), initial).expect("write initial YAML");

    let guard = lock_yaml_file(&scan_dir, filename)
        .await
        .expect("lock YAML file");
    let scan_dir_for_task = scan_dir.clone();
    let read_task =
        tokio::spawn(async move { read_yaml_for_scan(&scan_dir_for_task, filename).await });

    tokio::task::yield_now().await;
    std::fs::write(dir.path().join(filename), replacement).expect("replace YAML");
    drop(guard);

    let (read, _guard) = read_task.await.expect("scanner task").expect("read YAML");
    assert_eq!(read, replacement);
}

#[test]
fn rejects_path_separators_and_traversal() {
    assert!(validate_yaml_filename("../evil.yml").is_err());
    assert!(validate_yaml_filename("sub/evil.yml").is_err());
    assert!(validate_yaml_filename("evil\\x.yml").is_err());
    assert!(validate_yaml_filename("").is_err());
    assert!(validate_yaml_filename("noext").is_err());
}

#[test]
fn accepts_safe_basenames() {
    assert!(validate_yaml_filename("hello-world.yml").is_ok());
    assert!(validate_yaml_filename("Hello_World_2.yaml").is_ok());
}

#[test]
fn rejects_dots_in_stem() {
    assert!(validate_yaml_filename("Hello_World.2.yaml").is_err());
    assert!(validate_yaml_filename("a.b.yml").is_err());
    assert!(validate_yaml_filename("sd-fad.-sf.yaml").is_err());
}

#[test]
fn slugifies_pipeline_name() {
    assert_eq!(
        filename_from_pipeline_name("Hello World"),
        "hello-world.yml"
    );
    assert_eq!(filename_from_pipeline_name("  "), "pipeline.yml");
    assert_eq!(filename_from_pipeline_name("sd -fad. sf"), "sd-fad-sf.yml");
}

#[test]
fn storage_path_is_basename_only() {
    let p = yaml_path_storage("./pipelines", "demo.yml").expect("path");
    assert_eq!(p, "demo.yml");
}

#[test]
fn normalize_strips_directories() {
    assert_eq!(
        normalize_yaml_path_key("/data/pipelines/demo.yml"),
        "demo.yml"
    );
    assert_eq!(normalize_yaml_path_key("./pipelines/demo.yml"), "demo.yml");
    assert_eq!(normalize_yaml_path_key("demo.yml"), "demo.yml");
}

#[test]
fn etag_stable_for_same_content() {
    let a = yaml_content_etag("name: x\nsteps: []\n");
    let b = yaml_content_etag("name: x\nsteps: []\n");
    assert_eq!(a, b);
    assert_ne!(a, yaml_content_etag("name: y\nsteps: []\n"));
}
