use crate::services::pipeline_scanner::{
    filename_from_pipeline_name, normalize_yaml_path_key, validate_yaml_filename,
    yaml_content_etag, yaml_path_storage,
};

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
    assert_eq!(
        filename_from_pipeline_name("sd -fad. sf"),
        "sd-fad-sf.yml"
    );
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
