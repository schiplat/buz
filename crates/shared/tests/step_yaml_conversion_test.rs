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
registry_secret: ${secrets.REGISTRY_AUTH}
dir: /workspace
timeout: 5m
retry: 2
continue-on-error: true
secrets: [UPLOAD_TOKEN]
env:
  REGION: cn-hangzhou
config:
  source: ${secrets.OSS_AUTH}
  key_path: releases/app
  dest: /opt/app
  render:
    template: app.env.tpl
    output: app.env
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
    assert_eq!(
        step.registry_secret.as_deref(),
        Some("${secrets.REGISTRY_AUTH}")
    );
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
    assert_eq!(step.config_source.as_deref(), Some("${secrets.OSS_AUTH}"));
    assert_eq!(step.config_key_path.as_deref(), Some("releases/app"));
    assert_eq!(step.config_dest.as_deref(), Some("/opt/app"));
    let render = step.config_render.as_ref().unwrap();
    assert_eq!(render.len(), 1);
    assert_eq!(render[0].template.as_deref(), Some("app.env.tpl"));
    assert_eq!(render[0].output.as_deref(), Some("app.env"));
}
