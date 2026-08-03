use shared::pipeline_yaml::parse_and_validate;
use std::fs;
use std::path::PathBuf;

#[test]
fn all_example_pipelines_parse_and_validate() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../example/pipelines");
    let mut failed = Vec::new();
    for entry in fs::read_dir(&dir).expect("example/pipelines") {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yml") {
            continue;
        }
        let yaml = fs::read_to_string(&path).unwrap();
        let r = parse_and_validate(&yaml);
        if !r.valid {
            failed.push(format!(
                "{}: {:?}",
                path.file_name().unwrap().to_string_lossy(),
                r.errors
            ));
        }
    }
    assert!(
        failed.is_empty(),
        "invalid example pipelines:\n{}",
        failed.join("\n")
    );
}
