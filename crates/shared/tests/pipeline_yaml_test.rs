use shared::pipeline_yaml::parse_and_validate;

#[test]
fn valid_minimal_yaml() {
    let yaml = r#"
name: hello-world
steps:
  - name: greet
    run: echo hello
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid);
    assert_eq!(r.errors.len(), 0);
}

#[test]
fn team_slug_parses_and_owner_group_alias() {
    let with_team = r#"
name: with-team
team: backend
steps:
  - name: a
    run: echo a
"#;
    let parsed: shared::pipeline_yaml::PipelineYaml =
        serde_yaml::from_str(with_team).expect("parse team");
    assert_eq!(parsed.team.as_deref(), Some("backend"));

    let with_alias = r#"
name: with-alias
owner_group: platform
steps:
  - name: a
    run: echo a
"#;
    let parsed: shared::pipeline_yaml::PipelineYaml =
        serde_yaml::from_str(with_alias).expect("parse owner_group alias");
    assert_eq!(parsed.team.as_deref(), Some("platform"));
}

#[test]
fn missing_name() {
    let yaml = r#"
name: ""
steps:
  - run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(r.errors.iter().any(|e| e.field == "name"));
}

#[test]
fn empty_steps() {
    let yaml = "name: test\nsteps: []\n";
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
}

#[test]
fn step_missing_command() {
    let yaml = r#"
name: test
steps:
  - name: noop
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
}

#[test]
fn ssh_without_host() {
    let yaml = r#"
name: deploy
steps:
  - name: push
    uses: ssh
    run: docker push
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
}

#[test]
fn ssh_multi_host() {
    let yaml = r#"
name: multi-deploy
steps:
  - name: push
    uses: ssh
    host:
      - prod-01
      - prod-02
    run: docker push
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
}

#[test]
fn full_example() {
    let yaml = r#"
name: Deploy Production
description: Build and deploy to production
on:
  manual:
    inputs:
      version:
        description: Version to deploy
        required: true
        default: latest
concurrency: deploy-prod
runner:
  labels: [prod, linux-x64]
  timeout: 60m
env:
  APP_NAME: myapp
variables:
  - BUILD_CACHE_DIR
secrets:
  - DOCKER_PASSWORD
steps:
  - name: checkout
    run: git pull origin main
  - name: build
    run: docker build -t ${APP_NAME} .
    timeout: 10m
  - name: deploy
    uses: ssh
    host: prod-01
    run: docker-compose up -d
    retry: 1
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(r.parsed.is_some());
    let parsed = r.parsed.unwrap();
    // Verify variables are parsed
    assert_eq!(parsed.variables, Some(vec!["BUILD_CACHE_DIR".to_string()]));
    // Verify env and secrets
    assert!(parsed.env.is_some());
    assert!(parsed.secrets.is_some());
}

#[test]
fn webhook_trigger() {
    let yaml = r#"
name: Webhook Deploy
on:
  webhook:
    path: deploy-prod
steps:
  - name: echo
    run: echo triggered
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(r.parsed.is_some());
    let parsed = r.parsed.unwrap();
    assert_eq!(parsed.trigger_type.as_deref(), Some("webhook"));
    let tc = parsed.trigger_config.unwrap();
    assert_eq!(tc.webhook_path.as_deref(), Some("deploy-prod"));
}

#[test]
fn cron_trigger() {
    let yaml = r#"
name: Nightly Backup
on:
  cron:
    schedule: "0 2 * * *"
steps:
  - name: backup
    run: pg_dump > backup.sql
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(r.parsed.is_some());
    let parsed = r.parsed.unwrap();
    assert_eq!(parsed.trigger_type.as_deref(), Some("cron"));
    let tc = parsed.trigger_config.unwrap();
    assert_eq!(tc.cron_expr.as_deref(), Some("0 2 * * *"));
}

#[test]
fn host_labels_parsing() {
    let yaml = r#"
name: Labeled Deploy
steps:
  - name: deploy
    uses: ssh
    host_labels: [prod, web]
    run: deploy.sh
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(r.parsed.is_some());
    let parsed = r.parsed.unwrap();
    assert_eq!(parsed.steps[0].host_labels.as_deref(), Some("prod,web"));
}

#[test]
fn step_work_dir_alias() {
    let yaml = r#"
name: Dir Test
steps:
  - name: build
    run: cargo build
    work_dir: ./src
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(r.parsed.is_some());
    let parsed = r.parsed.unwrap();
    assert_eq!(parsed.steps[0].dir.as_deref(), Some("./src"));
}

#[test]
fn step_dir_alias() {
    let yaml = r#"
name: Dir Test
steps:
  - name: build
    run: cargo build
    dir: ./src
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(r.parsed.is_some());
    let parsed = r.parsed.unwrap();
    assert_eq!(parsed.steps[0].dir.as_deref(), Some("./src"));
}

#[test]
fn wrong_trigger_type_in_yaml_parses_as_manual() {
    let yaml = r#"
name: Default Trigger
steps:
  - name: hello
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid);
    assert!(r.parsed.is_some());
    let parsed = r.parsed.unwrap();
    assert_eq!(parsed.trigger_type.as_deref(), Some("manual"));
}
