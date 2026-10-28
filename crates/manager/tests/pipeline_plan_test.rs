use manager::pipeline_plan::plan;
use sha2::{Digest, Sha256};
use std::path::Path;

fn write_plugin_index(dir: &Path) {
    std::fs::write(
        dir.join("shell.yaml"),
        r#"id: buz/http-check
publisher: official
author: Buz
source_repo: https://example.invalid/shell.yaml
version: 1.0.0
engine: shell
inputs:
  url:
    type: string
    required: true
run: echo "$INPUT_URL"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("docker.yaml"),
        r#"id: buz/oss-upload
publisher: official
author: Buz
source_repo: https://example.invalid/docker.yaml
version: 1.0.0
engine: docker
image: docker.io/example/oss@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
inputs:
  source:
    type: string
    required: true
  access_key_id:
    type: string
    required: true
    masking: true
run: echo "$INPUT_SOURCE"
"#,
    )
    .unwrap();
}

fn pipeline_yaml() -> &'static str {
    r#"name: Plugin plan test
environments: [default]
steps:
  - name: Check endpoint
    uses: buz/http-check@1.0.0
    with:
      url: https://example.invalid/health
  - name: Upload artifact
    uses: buz/oss-upload@1.0.0
    with:
      source: dist/app.tar.gz
      access_key_id: ${secrets.TEST_OSS_KEY}
"#
}

#[test]
fn plan_resolves_shell_and_docker_without_revealing_inputs_or_scripts() {
    let index = tempfile::tempdir().unwrap();
    write_plugin_index(index.path());
    let (output, images) = plan(pipeline_yaml(), index.path(), None, false).unwrap();
    assert!(output.contains("1. Check endpoint\n   engine: shell"));
    assert!(output.contains("uses: buz/http-check@1.0.0"));
    assert!(output.contains("2. Upload artifact\n   engine: docker"));
    assert!(output.contains("uses: buz/oss-upload@1.0.0"));
    assert!(output.contains("resolved version: 1.0.0"));
    assert!(output.contains("image: docker.io/example/oss@sha256:"));
    assert!(output.contains("inputs: ACCESS_KEY_ID, SOURCE"));
    assert!(!output.contains("TEST_OSS_KEY"));
    assert!(!output.contains("echo \"$INPUT_"));
    assert!(output.contains("DRY RUN ONLY"));
    assert_eq!(images.len(), 1);
    assert!(images[0].starts_with("docker.io/example/oss@sha256:"));
}

#[test]
fn plan_rejects_invalid_yaml_missing_plugins_and_bad_inputs() {
    let index = tempfile::tempdir().unwrap();
    write_plugin_index(index.path());
    assert!(plan("name: [invalid", index.path(), None, false).is_err());
    assert!(plan(
            "name: Missing\nenvironments: [default]\nsteps:\n  - name: Missing\n    uses: buz/missing@1.0.0\n",
            index.path(),
            None,
            false
        )
        .unwrap_err()
        .contains("not found in the local plugin index"));
    let missing_input =
            "name: Missing input\nenvironments: [default]\nsteps:\n  - name: Upload\n    uses: buz/oss-upload@1.0.0\n";
    assert!(plan(missing_input, index.path(), None, false)
        .unwrap_err()
        .contains("missing required input 'source'"));
    let unknown_input = "name: Unknown input\nenvironments: [default]\nsteps:\n  - name: Check\n    uses: buz/http-check@1.0.0\n    with:\n      url: https://example.invalid\n      undeclared: value\n";
    assert!(plan(unknown_input, index.path(), None, false)
        .unwrap_err()
        .contains("with.undeclared: template 'buz/http-check' does not declare this input"));
}

#[test]
fn strict_review_policy_requires_and_accepts_manifest_hashes() {
    let index = tempfile::tempdir().unwrap();
    write_plugin_index(index.path());
    assert!(plan(pipeline_yaml(), index.path(), None, true)
        .unwrap_err()
        .contains("not officially reviewed"));

    let shell = std::fs::read(index.path().join("shell.yaml")).unwrap();
    let docker = std::fs::read(index.path().join("docker.yaml")).unwrap();
    let hashes = [shell, docker]
        .iter()
        .map(|raw| format!("{:x}", Sha256::digest(raw)))
        .collect::<Vec<_>>();
    let manifest = index.path().join("reviews.json");
    std::fs::write(
            &manifest,
            serde_json::json!({"reviews": hashes.iter().map(|sha256| serde_json::json!({"sha256": sha256})).collect::<Vec<_>>()})
                .to_string(),
        )
        .unwrap();
    let (output, images) = plan(pipeline_yaml(), index.path(), Some(&manifest), true).unwrap();
    assert_eq!(output.matches("reviewed: yes").count(), 2);
    assert_eq!(images.len(), 1);
}

#[test]
fn direct_docker_steps_are_included_in_image_check_list() {
    let index = tempfile::tempdir().unwrap();
    let raw = r#"name: Direct Docker
environments: [default]
steps:
  - name: Inspect
    engine: docker
    image: busybox:1.36
    run: echo ok
"#;
    let (output, images) = plan(raw, index.path(), None, false).unwrap();
    assert!(output.contains("engine: docker\n   image: busybox:1.36"));
    assert_eq!(images, ["busybox:1.36"]);
}
