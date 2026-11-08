use shared::pipeline_yaml::StepYaml;
use shared::protocol::StepDef;

#[test]
fn conversion_preserves_step_fields_and_flattens_config() {
    let parsed: StepYaml = serde_yaml::from_str(
        r#"
name: Upload artifact
run: echo ok
engine: docker
image: example/image:1
volumes: ["/tmp:/tmp"]
privileged: true
dir: /workspace
timeout: 5m
retry: 2
continue-on-error: true
secrets: [UPLOAD_TOKEN]
env:
  REGION: cn-hangzhou
"#,
    )
    .unwrap();

    let step = StepDef::from(&parsed);
    assert_eq!(step.name, "Upload artifact");
    assert_eq!(step.run, "echo ok");
    assert_eq!(step.engine.as_deref(), Some("docker"));
    assert_eq!(step.image.as_deref(), Some("example/image:1"));
    assert_eq!(
        step.volumes.as_deref(),
        Some(&["/tmp:/tmp".to_string()][..])
    );
    assert_eq!(step.privileged, Some(true));
    assert_eq!(step.dir.as_deref(), Some("/workspace"));
    assert_eq!(step.timeout.as_deref(), Some("5m"));
    assert_eq!(step.retry, Some(2));
    assert_eq!(step.continue_on_error, Some(true));
    assert_eq!(
        step.secrets.as_deref(),
        Some(&["UPLOAD_TOKEN".to_string()][..])
    );
    assert_eq!(
        step.env.as_ref().unwrap().get("REGION").unwrap(),
        "cn-hangzhou"
    );
}
