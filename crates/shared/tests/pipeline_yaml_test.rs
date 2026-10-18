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
    assert!(
        r.errors
            .iter()
            .any(|e| e.field.contains("run") || e.message.contains("'run'")),
        "{:?}",
        r.errors
    );
}

#[test]
fn step_typo_ru_instead_of_run_rejected() {
    // `engine: shell` previously satisfied "run or uses", while unknown `ru:` was ignored.
    let yaml = r#"
name: test
steps:
  - name: Render env
    engine: shell
    ru: |
      echo "==> BUZ_CONFIG_DIR: ${BUZ_CONFIG_DIR:-<unset>}"
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid, "typo `ru:` must fail validation");
    let msg = r
        .errors
        .iter()
        .map(|e| format!("{}: {}", e.field, e.message))
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        msg.contains("ru") || msg.contains("'run'"),
        "expected unknown-field or missing-run error, got: {msg}"
    );
    // Navigable field for editor highlight (path + bad key).
    assert!(
        r.errors
            .iter()
            .any(|e| e.field == "steps[0].ru" || e.field.ends_with(".ru") || e.field == "ru"),
        "field should name the bad key for highlighting, got: {msg}"
    );
}

#[test]
fn uses_shell_without_run_rejected() {
    let yaml = r#"
name: test
steps:
  - name: noop
    engine: shell
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors.iter().any(|e| e.message.contains("'run'")),
        "{:?}",
        r.errors
    );
}

fn assert_unknown_key_rejected(yaml: &str, unknown_token: &str) {
    let r = parse_and_validate(yaml);
    assert!(
        !r.valid,
        "expected rejection for unknown key `{unknown_token}`"
    );
    let msg = r
        .errors
        .iter()
        .map(|e| format!("{}: {}", e.field, e.message))
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        msg.contains("unknown field") && msg.contains(unknown_token),
        "expected unknown field `{unknown_token}` in errors, got: {msg}"
    );
}

#[test]
fn unknown_top_level_key_rejected() {
    assert_unknown_key_rejected(
        r#"
name: test
runners:
  labels: [linux]
steps:
  - name: a
    run: echo hi
"#,
        "runners",
    );
}

#[test]
fn unknown_runner_key_rejected() {
    assert_unknown_key_rejected(
        r#"
name: test
runner:
  label: [linux]
  timeout: 5m
steps:
  - name: a
    run: echo hi
"#,
        "label",
    );
}

#[test]
fn unknown_concurrency_key_rejected() {
    assert_unknown_key_rejected(
        r#"
name: test
concurrency:
  grup: deploy
  mode: queue
steps:
  - name: a
    run: echo hi
"#,
        "grup",
    );
}

#[test]
fn unknown_on_trigger_key_rejected() {
    assert_unknown_key_rejected(
        r#"
name: test
on:
  push:
    branches: [main]
steps:
  - name: a
    run: echo hi
"#,
        "push",
    );
}

#[test]
fn unknown_manual_input_key_rejected() {
    assert_unknown_key_rejected(
        r#"
name: test
on:
  manual:
    inputs:
      tag:
        description: Tag
        require: true
steps:
  - name: a
    run: echo hi
"#,
        "require",
    );
}

#[test]
fn unknown_config_key_rejected() {
    assert_unknown_key_rejected(
        r#"
name: test
steps:
  - name: a
    engine: shell
    config:
      dest: /tmp/x
      renders:
        - template: env.example
    run: echo hi
"#,
        "renders",
    );
}

#[test]
fn unknown_config_render_key_rejected() {
    assert_unknown_key_rejected(
        r#"
name: test
steps:
  - name: a
    engine: shell
    config:
      dest: /tmp/x
      render:
        template: env.example
        out: .env
    run: echo hi
"#,
        "out",
    );
}

#[test]
fn invalid_uses_engine_rejected() {
    let yaml = r#"
name: test
steps:
  - name: a
    engine: shel
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.field.contains("engine") && e.message.contains("shel")),
        "{:?}",
        r.errors
    );
}

#[test]
fn concurrency_kebab_aliases_accepted() {
    let yaml = r#"
name: test
concurrency:
  group: deploy
  mode: queue
  max-queue: 5
  queue-timeout: 10m
steps:
  - name: a
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "{:?}", r.errors);
}

#[test]
fn ssh_without_host() {
    let yaml = r#"
name: deploy
steps:
  - name: push
    engine: ssh
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
    engine: ssh
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
        type: string
        pattern: "^[A-Za-z0-9._-]+$"
      deploy_token:
        description: One-time token
        required: true
        masking: true
      environment:
        description: Target env
        type: choice
        options: [staging, production]
      replicas:
        type: number
        default: "3"
concurrency:
  group: deploy-prod
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
    engine: ssh
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
    let inputs = parsed
        .trigger_config
        .as_ref()
        .and_then(|c| c.inputs.as_ref())
        .expect("inputs");
    let token = inputs.iter().find(|i| i.name == "deploy_token").unwrap();
    assert_eq!(token.masking, Some(true));
    let version = inputs.iter().find(|i| i.name == "version").unwrap();
    assert_eq!(version.input_type.as_deref(), Some("string"));
    assert_eq!(version.pattern.as_deref(), Some("^[A-Za-z0-9._-]+$"));
    let env = inputs.iter().find(|i| i.name == "environment").unwrap();
    assert_eq!(env.input_type.as_deref(), Some("choice"));
    assert_eq!(
        env.options.as_deref(),
        Some(["staging".to_string(), "production".to_string()].as_slice())
    );
    let replicas = inputs.iter().find(|i| i.name == "replicas").unwrap();
    assert_eq!(replicas.input_type.as_deref(), Some("number"));
    assert_eq!(replicas.default.as_deref(), Some("3"));
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
    engine: ssh
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
fn hosts_strategy_chain_default_and_parallel_opt_in() {
    let omitted = r#"
name: Multi
steps:
  - name: deploy
    engine: ssh
    host: a
    run: true
"#;
    let r = parse_and_validate(omitted);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(
        r.parsed.as_ref().unwrap().steps[0].hosts_strategy.is_none(),
        "omitted stays None in parsed; runtime normalizes to chain"
    );
    assert_eq!(
        shared::pipeline_yaml::normalize_hosts_strategy(None).unwrap(),
        "chain"
    );
    assert_eq!(
        shared::pipeline_yaml::normalize_hosts_strategy(Some("serial")).unwrap(),
        "chain",
        "legacy serial alias"
    );

    let parallel = r#"
name: Multi
steps:
  - name: deploy
    engine: ssh
    host: a
    hosts_strategy: parallel
    run: true
"#;
    let r2 = parse_and_validate(parallel);
    assert!(r2.valid, "Errors: {:?}", r2.errors);
    assert_eq!(
        r2.parsed.as_ref().unwrap().steps[0]
            .hosts_strategy
            .as_deref(),
        Some("parallel")
    );

    let chain = r#"
name: Multi
steps:
  - name: deploy
    engine: ssh
    host: a
    hosts_strategy: chain
    run: true
"#;
    let r3 = parse_and_validate(chain);
    assert!(r3.valid, "Errors: {:?}", r3.errors);
    assert_eq!(
        r3.parsed.as_ref().unwrap().steps[0]
            .hosts_strategy
            .as_deref(),
        Some("chain")
    );
}

#[test]
fn hosts_strategy_rejects_invalid_and_non_ssh() {
    let bad = r#"
name: Bad
steps:
  - name: d
    engine: ssh
    host: a
    hosts_strategy: zigzag
    run: true
"#;
    let r = parse_and_validate(bad);
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.field.contains("hosts_strategy") && e.message.contains("parallel")),
        "{:?}",
        r.errors
    );

    let on_shell = r#"
name: Shell
steps:
  - name: s
    engine: shell
    hosts_strategy: parallel
    run: true
"#;
    let r2 = parse_and_validate(on_shell);
    assert!(!r2.valid);
    assert!(
        r2.errors
            .iter()
            .any(|e| e.message.contains("only valid for engine: ssh")),
        "{:?}",
        r2.errors
    );
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

#[test]
fn docker_step_with_image_valid() {
    let yaml = r#"
name: Docker Smoke
steps:
  - name: hello
    engine: docker
    image: alpine:3.20
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    let parsed = r.parsed.unwrap();
    assert_eq!(parsed.steps[0].engine.as_deref(), Some("docker"));
    assert_eq!(parsed.steps[0].image.as_deref(), Some("alpine:3.20"));
}

#[test]
fn docker_step_missing_image_rejected() {
    let yaml = r#"
name: Docker Bad
steps:
  - name: hello
    engine: docker
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors.iter().any(|e| e.message.contains("image")),
        "Errors: {:?}",
        r.errors
    );
}

#[test]
fn docker_step_with_host_rejected() {
    let yaml = r#"
name: Docker Bad Host
steps:
  - name: hello
    engine: docker
    image: alpine:3.20
    host: prod-01
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.message.contains("host") || e.message.contains("host_labels")),
        "Errors: {:?}",
        r.errors
    );
}

#[test]
fn docker_step_with_privileged_valid() {
    let yaml = r#"
name: privileged-docker
steps:
  - name: dind
    engine: docker
    image: docker:27-dind
    privileged: true
    run: dockerd &
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
}

#[test]
fn non_docker_step_with_privileged_rejected() {
    let yaml = r#"
name: bad-privileged
steps:
  - name: shell-step
    engine: shell
    privileged: true
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors.iter().any(|e| e.message.contains("privileged")),
        "Errors: {:?}",
        r.errors
    );
}

#[test]
fn docker_step_with_registry_secret_valid() {
    let yaml = r#"
name: pull-private
steps:
  - name: run
    engine: docker
    image: crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/ns/app:1.0
    registry_secret: acr-personal
    run: echo ok
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    let parsed = r.parsed.unwrap();
    assert_eq!(
        parsed.steps[0].registry_secret.as_deref(),
        Some("acr-personal")
    );
}

#[test]
fn shell_step_with_registry_secret_valid() {
    let yaml = r#"
name: push-private
steps:
  - name: push
    engine: shell
    registry_secret: acr-personal
    run: docker push crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/ns/app:1.0
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert_eq!(
        r.parsed.unwrap().steps[0].registry_secret.as_deref(),
        Some("acr-personal")
    );
}

#[test]
fn concurrency_legacy_string_format() {
    let yaml = r#"
name: legacy-concurrency
concurrency: deploy-prod
steps:
  - name: hello
    run: echo hi
"#;
    let parsed: shared::pipeline_yaml::PipelineYaml =
        serde_yaml::from_str(yaml).expect("parse legacy concurrency");
    match parsed.concurrency {
        Some(shared::pipeline_yaml::ConcurrencyDef::Simple(g)) => {
            assert_eq!(g, "deploy-prod");
        }
        _ => panic!("expected Simple concurrency"),
    }
}

#[test]
fn concurrency_block_with_explicit_mode() {
    let yaml = r#"
name: queue-concurrency
concurrency:
  group: dind-build
  mode: queue
  max_queue: 5
  queue_timeout: 10m
steps:
  - name: build
    run: docker build .
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    let parsed = r.parsed.unwrap();
    assert_eq!(parsed.concurrency.as_deref(), Some("dind-build"));
    let cc = parsed.concurrency_config.unwrap();
    assert_eq!(cc.group, "dind-build");
    assert_eq!(cc.mode.as_deref(), Some("queue"));
    assert_eq!(cc.max_queue, Some(5));
    assert_eq!(cc.queue_timeout.as_deref(), Some("10m"));
}

#[test]
fn concurrency_block_defaults_to_discard_old() {
    let yaml = r#"
name: default-concurrency
concurrency:
  group: ci-build
steps:
  - name: test
    run: cargo test
"#;
    let parsed: shared::pipeline_yaml::PipelineYaml =
        serde_yaml::from_str(yaml).expect("parse default concurrency");
    match parsed.concurrency {
        Some(shared::pipeline_yaml::ConcurrencyDef::Full(fc)) => {
            assert_eq!(fc.group, "ci-build");
            assert_eq!(fc.mode, "discard-old", "mode should default to discard-old");
        }
        _ => panic!("expected Full concurrency block"),
    }
}

#[test]
fn concurrency_invalid_mode_rejected() {
    let yaml = r#"
name: bad-mode
concurrency:
  group: test
  mode: unknown-mode
steps:
  - name: hi
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors.iter().any(|e| e.field == "concurrency.mode"),
        "Errors: {:?}",
        r.errors
    );
}

#[test]
fn concurrency_full_example_with_block_format() {
    let yaml = r#"
name: CI Pipeline
description: Build with queue
concurrency:
  group: ci-linux
on:
  manual:
    inputs:
      version:
        description: Version tag
        required: true
runner:
  labels: [linux-x64, docker]
  timeout: 30m
env:
  APP_NAME: myservice
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
    engine: ssh
    host: prod-01
    run: docker-compose up -d
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(r.parsed.is_some());
    let parsed = r.parsed.unwrap();
    assert_eq!(parsed.concurrency.as_deref(), Some("ci-linux"));
    assert!(parsed.concurrency_config.is_some());
    assert_eq!(parsed.concurrency_config.unwrap().group, "ci-linux");
}

#[test]
fn masking_input_demo_example_file() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../example/pipelines/masking-input-demo.yml");
    let yaml = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("read {}: {e}", path.display());
    });
    let r = parse_and_validate(&yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    let inputs = r
        .parsed
        .as_ref()
        .and_then(|p| p.trigger_config.as_ref())
        .and_then(|c| c.inputs.as_ref())
        .expect("inputs");
    let token = inputs.iter().find(|i| i.name == "deploy_token").unwrap();
    assert_eq!(token.masking, Some(true));
    let version = inputs.iter().find(|i| i.name == "version").unwrap();
    assert!(version.masking.is_none() || version.masking == Some(false));
}

// ── Config block DSL v2 (Phase 2b / D1) ───────────────────────────────

#[test]
fn config_oss_deliver_accepted() {
    let yaml = r#"
name: oss-deliver
steps:
  - name: deliver
    engine: shell
    config:
      source: OSS_DEMO
      key_path: config_tpl/demo/v1
      dest: /tmp/cfg
    run: ls "$BUZ_CONFIG_DIR"
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("config.source: deprecated") && w.contains("buz/config-fetch")),
        "expected OSS delivery deprecation warning, got: {:?}",
        r.warnings
    );
    let step = &r.parsed.unwrap().steps[0];
    assert_eq!(step.config_source.as_deref(), Some("OSS_DEMO"));
    assert_eq!(step.config_key_path.as_deref(), Some("config_tpl/demo/v1"));
    assert_eq!(step.config_dest.as_deref(), Some("/tmp/cfg"));
    assert!(step.config_render.is_none());
    assert!(step.config.is_some());
}

#[test]
fn config_key_path_trailing_slash_ok_no_warning() {
    let yaml = r#"
name: oss-slash
steps:
  - name: deliver
    engine: shell
    config:
      source: OSS_DEMO
      key_path: config_tpl/demo/v1/
      dest: /tmp/cfg
    run: ls
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(
        !r.warnings.iter().any(|w| w.contains("key_path")),
        "trailing slash must not warn: {:?}",
        r.warnings
    );
}

#[test]
fn config_key_path_rejects_archive_filename() {
    let yaml = r#"
name: oss-archive
steps:
  - name: deliver
    engine: shell
    config:
      source: OSS_DEMO
      key_path: config_tpl/demo/v1/configs.tar.gz
      dest: /tmp/cfg
    run: ls
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.field.contains("key_path") && e.message.contains("directory prefix")),
        "errors: {:?}",
        r.errors
    );
}

#[test]
fn config_render_only_accepted() {
    let yaml = r#"
name: render-only
steps:
  - name: render
    engine: ssh
    host: host-b
    config:
      dest: /opt/manager
      render:
        template: env.example
        output: .env
        mode: "0600"
        on_missing: fail
        backup_keep: 2
    run: cat "$BUZ_CONFIG_DIR/.env"
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    let step = &r.parsed.unwrap().steps[0];
    assert!(step.config_source.is_none());
    assert_eq!(step.config_dest.as_deref(), Some("/opt/manager"));
    let render = step.config_render.as_ref().unwrap();
    assert_eq!(render.len(), 1);
    assert_eq!(render[0].template.as_deref(), Some("env.example"));
    assert_eq!(render[0].output.as_deref(), Some(".env"));
    assert_eq!(render[0].mode.as_deref(), Some("0600"));
    assert_eq!(render[0].on_missing.as_deref(), Some("fail"));
    assert_eq!(render[0].backup_keep, Some(2));
}

#[test]
fn config_render_list_normalizes_to_vec() {
    let yaml = r#"
name: multi-render
steps:
  - name: render
    engine: shell
    config:
      dest: /opt/manager
      render:
        - template: env.example
          output: .env
          mode: "0600"
          on_missing: fail
        - template: nginx.conf.tpl
          output: conf/nginx.conf
          mode: "0644"
          on_missing: warn
    run: echo ok
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    let parsed = r.parsed.unwrap();
    let render = parsed.steps[0].config_render.as_ref().unwrap();
    assert_eq!(render.len(), 2);
    assert_eq!(render[1].template.as_deref(), Some("nginx.conf.tpl"));
    assert_eq!(render[1].on_missing.as_deref(), Some("warn"));
}

#[test]
fn config_sugar_deliver_plus_render_allowed() {
    let yaml = r#"
name: sugar
steps:
  - name: all-in-one
    engine: shell
    config:
      source: OSS_CONFIG
      key_path: config_tpl/x/v1
      dest: /opt/manager
      render:
        - template: env.example
          output: .env
          mode: "0600"
          on_missing: fail
    run: echo ok
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    let step = &r.parsed.unwrap().steps[0];
    assert_eq!(step.config_source.as_deref(), Some("OSS_CONFIG"));
    assert_eq!(step.config_render.as_ref().unwrap().len(), 1);
}

#[test]
fn config_rejects_legacy_flat_fields() {
    let yaml = r#"
name: legacy
steps:
  - name: old
    engine: shell
    config_source: OSS_DEMO
    config_path: config_tpl/demo/v1
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors.iter().any(|e| e.message.contains("config:")),
        "Errors: {:?}",
        r.errors
    );
}

#[test]
fn config_rejects_source_without_key_path() {
    let yaml = r#"
name: bad-deliver
steps:
  - name: x
    engine: shell
    config:
      source: OSS_DEMO
      dest: /tmp/x
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.message.contains("source") && e.message.contains("key_path")),
        "Errors: {:?}",
        r.errors
    );
}

#[test]
fn config_rejects_render_without_dest() {
    let yaml = r#"
name: bad-render
steps:
  - name: x
    engine: shell
    config:
      render:
        template: env.example
        output: .env
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors.iter().any(|e| e.message.contains("dest")),
        "Errors: {:?}",
        r.errors
    );
}

#[test]
fn config_rejects_dest_alone() {
    let yaml = r#"
name: dest-only
steps:
  - name: x
    engine: shell
    config:
      dest: /opt/manager
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
}

#[test]
fn config_rejects_render_missing_template() {
    let yaml = r#"
name: no-template
steps:
  - name: x
    engine: shell
    config:
      dest: /opt/manager
      render:
        output: .env
        on_missing: fail
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors.iter().any(|e| e.message.contains("template")),
        "Errors: {:?}",
        r.errors
    );
}

#[test]
fn config_rejects_bad_on_missing() {
    let yaml = r#"
name: bad-om
steps:
  - name: x
    engine: shell
    config:
      dest: /opt/manager
      render:
        template: env.example
        on_missing: ignore
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors.iter().any(|e| e.message.contains("on_missing")),
        "Errors: {:?}",
        r.errors
    );
}

#[test]
fn config_render_not_allowed_on_docker() {
    let yaml = r#"
name: docker-render
steps:
  - name: x
    engine: docker
    image: alpine:3
    config:
      dest: /opt/manager
      render:
        template: env.example
        output: .env
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors.iter().any(|e| e.message.contains("render")),
        "Errors: {:?}",
        r.errors
    );
}

#[test]
fn step_env_map_and_secrets_expr_slot_parsed() {
    let yaml = r#"
name: gha-secrets
secrets:
  - GIT_SSH_KEY
  - ACR_PERSONAL
steps:
  - name: clone
    secrets: [GIT_SSH_KEY]
    env:
      GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}
    run: echo "$GIT_SSH_KEY" > /tmp/key
  - name: push
    engine: shell
    secrets: [ACR_PERSONAL]
    registry_secret: ${secrets.ACR_PERSONAL}
    run: docker push example.com/app:1
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    let parsed = r.parsed.unwrap();
    let env = parsed.steps[0].env.as_ref().unwrap();
    assert_eq!(
        env.get("GIT_SSH_KEY").map(String::as_str),
        Some("${secrets.GIT_SSH_KEY}")
    );
    assert_eq!(
        parsed.steps[1].registry_secret.as_deref(),
        Some("${secrets.ACR_PERSONAL}")
    );
}

#[test]
fn run_must_not_contain_secrets_expr() {
    let yaml = r#"
name: bad-run
steps:
  - name: clone
    secrets: [GIT_SSH_KEY]
    run: echo "${secrets.GIT_SSH_KEY}"
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.message.contains("${secrets.…}") || e.message.contains("env:")),
        "Errors: {:?}",
        r.errors
    );
}

#[test]
fn invalid_timeout_with_spaces_or_garbage_rejected() {
    let yaml = r#"
name: bad-timeout
steps:
  - name: a
    run: echo hi
    timeout: 10x m
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.field == "steps[0].timeout" && e.message.contains("10x m")),
        "errors: {:?}",
        r.errors
    );
}

#[test]
fn timeout_formats_accepted() {
    for t in ["30", "30s", "10m", "1h", "2H"] {
        let yaml = format!(
            r#"
name: ok-timeout
runner:
  timeout: {t}
steps:
  - name: a
    run: echo hi
    timeout: {t}
"#
        );
        let r = parse_and_validate(&yaml);
        assert!(r.valid, "timeout={t} errors: {:?}", r.errors);
    }
}

#[test]
fn parse_duration_seconds_helpers() {
    use shared::pipeline_yaml::parse_duration_seconds;
    assert_eq!(parse_duration_seconds("10m").unwrap(), 600);
    assert_eq!(parse_duration_seconds("300s").unwrap(), 300);
    assert_eq!(parse_duration_seconds("1h").unwrap(), 3600);
    assert_eq!(parse_duration_seconds("45").unwrap(), 45);
    assert!(parse_duration_seconds("10x m").is_err());
    assert!(parse_duration_seconds("10xm").is_err());
    assert!(parse_duration_seconds("0m").is_err());
}

#[test]
fn collect_effective_secret_names_from_refs_without_top_level() {
    use shared::pipeline_yaml::collect_effective_secret_names;

    let yaml = r#"
name: thin-top
steps:
  - name: clone
    env:
      GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}
    run: echo "$GIT_SSH_KEY"
  - name: push
    engine: shell
    registry_secret: ${secrets.ACR_PERSONAL}
    run: docker push example.com/app:1
  - name: deliver
    engine: ssh
    host: box
    config:
      source: ${secrets.OSS_CONFIG}
      key_path: tpl/v1
      dest: /opt/app
    run: ls
"#;
    let parsed: shared::pipeline_yaml::PipelineYaml = serde_yaml::from_str(yaml).expect("parse");
    let names = collect_effective_secret_names(&parsed);
    assert_eq!(
        names,
        vec![
            "ACR_PERSONAL".to_string(),
            "GIT_SSH_KEY".to_string(),
            "OSS_CONFIG".to_string(),
        ]
    );
}

#[test]
fn notify_feishu_parses_and_defaults() {
    use shared::pipeline_yaml::collect_effective_secret_names;
    let yaml = r#"
name: with-notify
notify:
  - type: feishu
    webhook: ${secrets.FEISHU_WEBHOOK}
steps:
  - name: ok
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "{:?}", r.errors);
    let parsed = r.parsed.unwrap();
    let notify = parsed.notify.unwrap();
    assert_eq!(notify.len(), 1);
    assert_eq!(notify[0].channel_type, "feishu");
    assert_eq!(notify[0].webhook, "${secrets.FEISHU_WEBHOOK}");
    assert!(notify[0].on.is_none());

    let py: shared::pipeline_yaml::PipelineYaml = serde_yaml::from_str(yaml).unwrap();
    let names = collect_effective_secret_names(&py);
    assert_eq!(names, vec!["FEISHU_WEBHOOK".to_string()]);
}

#[test]
fn notify_unknown_type_rejected() {
    let yaml = r#"
name: bad-notify
notify:
  - type: slack
    webhook: ${secrets.HOOK}
steps:
  - run: echo x
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.field.contains("notify") && e.field.contains("type")),
        "{:?}",
        r.errors
    );
}

#[test]
fn notify_unknown_key_rejected() {
    let yaml = r#"
name: bad-notify-key
notify:
  - type: feishu
    webhook: ${secrets.HOOK}
    extra: nope
steps:
  - run: echo x
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors.iter().any(|e| e.message.contains("unknown field")),
        "{:?}",
        r.errors
    );
}

#[test]
fn notify_invalid_on_and_webhook_rejected() {
    let yaml = r#"
name: bad-notify-on
notify:
  - type: feishu
    webhook: https://plain.example/hook
    on: [success, boom]
steps:
  - run: echo x
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors.iter().any(|e| e.field.contains("webhook")),
        "{:?}",
        r.errors
    );
    assert!(
        r.errors.iter().any(|e| e.field.contains("on")),
        "{:?}",
        r.errors
    );
}

#[test]
fn notify_should_send_helpers() {
    use shared::pipeline_yaml::notify_should_send;
    assert!(notify_should_send(None, "success"));
    assert!(!notify_should_send(None, "cancelled"));
    let on = vec!["cancelled".into()];
    assert!(notify_should_send(Some(&on), "cancelled"));
}

// ── Manual input type reminders ──────────────────────────────────────

fn warnings_for(yaml: &str) -> Vec<String> {
    let r = shared::pipeline_yaml::parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    r.warnings
}

#[test]
fn input_without_type_gets_reminder_warning() {
    let yaml = r#"
name: remind
steps:
  - name: run
    engine: shell
    run: echo hi
on:
  manual:
    inputs:
      tag:
        description: ACR tag
      replicas:
        type: number
"#;
    let warnings = warnings_for(yaml);
    let tag = warnings
        .iter()
        .find(|w| w.starts_with("on.manual.inputs.tag:"))
        .expect("tag warning");
    assert!(tag.contains("no 'type' declared"), "warning: {tag}");
    assert!(
        !warnings.iter().any(|w| w.contains("replicas")),
        "typed input must not warn: {warnings:?}"
    );
}

#[test]
fn options_without_choice_gets_specific_warning() {
    let yaml = r#"
name: remind-choice
steps:
  - name: run
    engine: shell
    run: echo hi
on:
  manual:
    inputs:
      env:
        options: [staging, production]
      dry_run:
        type: boolean
"#;
    let warnings = warnings_for(yaml);
    let env = warnings
        .iter()
        .find(|w| w.starts_with("on.manual.inputs.env:"))
        .expect("env warning");
    assert!(
        env.contains("requires 'type: choice'"),
        "options-specific message: {env}"
    );
    assert!(
        !env.contains("no 'type' declared"),
        "options case must not also emit the generic reminder: {env}"
    );
    assert!(
        !warnings.iter().any(|w| w.contains("dry_run")),
        "{warnings:?}"
    );
}

#[test]
fn choice_with_options_is_quiet() {
    let yaml = r#"
name: quiet
steps:
  - name: run
    engine: shell
    run: echo hi
on:
  manual:
    inputs:
      env:
        type: choice
        options: [staging, production]
      replicas:
        type: number
"#;
    let warnings = warnings_for(yaml);
    assert!(
        warnings.iter().all(|w| !w.contains("on.manual.inputs.")),
        "typed inputs must not warn: {warnings:?}"
    );
}

#[test]
fn retry_declared_gets_not_executed_warning() {
    let yaml = r#"
name: retry-warning
steps:
  - name: flaky
    engine: shell
    run: echo hi
    retry: 2
"#;
    let warnings = warnings_for(yaml);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("steps[0].retry") && w.contains("not executed yet")),
        "retry>0 must warn: {warnings:?}"
    );

    let quiet = r#"
name: retry-zero
steps:
  - name: once
    engine: shell
    run: echo hi
    retry: 0
"#;
    let warnings = warnings_for(quiet);
    assert!(
        warnings.iter().all(|w| !w.contains(".retry")),
        "retry=0 must not warn: {warnings:?}"
    );
}

#[test]
fn render_on_change_and_services_get_not_enforced_warning() {
    let yaml = r#"
name: render-change
steps:
  - name: render
    engine: shell
    run: echo hi
    config:
      dest: ./app
      render:
        - template: env.example
          output: .env
          on_change: recreate
          services: [web, worker]
"#;
    let warnings = warnings_for(yaml);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("config.render[0]") && w.contains("not enforced")),
        "on_change/services must warn: {warnings:?}"
    );
}

#[test]
fn uses_step_with_inputs_is_valid() {
    let yaml = r#"
name: uses-step
steps:
  - name: notify
    uses: buz/feishu@1.2
    with:
      webhook: ${secrets.FEISHU_WEBHOOK}
      title: deploy done
"#;
    let r = shared::pipeline_yaml::parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
}

#[test]
fn uses_rejects_image_override_but_allows_engine_and_run() {
    let yaml = r#"
name: uses-conflict
steps:
  - name: notify
    uses: buz/feishu@1.2
    image: alpine:3
"#;
    let r = shared::pipeline_yaml::parse_and_validate(yaml);
    assert!(!r.valid, "image must conflict with uses");
    assert!(
        r.errors.iter().any(|e| e.field == "steps[0].image"),
        "{:?}",
        r.errors
    );

    // `engine:` is a deployment-context override — statically allowed;
    // the resolver enforces the template's declared engines at save time.
    // `run:` is appended after the template run block.
    let yaml = r#"
name: uses-override
steps:
  - name: unpack
    uses: buz/archive@1.1
    engine: ssh
    host: $deploy_host
    run: echo appended
"#;
    let r = shared::pipeline_yaml::parse_and_validate(yaml);
    assert!(
        r.valid,
        "engine/run with uses must be valid: {:?}",
        r.errors
    );
}

#[test]
fn with_requires_uses() {
    let yaml = r#"
name: orphan-with
steps:
  - name: run
    engine: shell
    run: echo hi
    with:
      key: value
"#;
    let r = shared::pipeline_yaml::parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors.iter().any(|e| e.field == "steps[0].with"),
        "{:?}",
        r.errors
    );
}

#[test]
fn uses_pin_parsing() {
    use shared::pipeline_yaml::{parse_uses_reference, UsesPin};

    let (reference, pin) = parse_uses_reference("buz/feishu@1.2").unwrap();
    assert_eq!(reference, "buz/feishu");
    assert_eq!(pin, UsesPin::Version("1.2".into()));

    let (_, pin) = parse_uses_reference("ops/ship@1").unwrap();
    assert_eq!(pin, UsesPin::Version("1".into()));

    let (_, pin) = parse_uses_reference("ops/ship@1.2.0").unwrap();
    assert_eq!(pin, UsesPin::Version("1.2.0".into()));

    let sha = format!("ops/ship@sha256:{}", "a".repeat(64));
    let (_, pin) = parse_uses_reference(&sha).unwrap();
    assert_eq!(pin, UsesPin::Sha256("a".repeat(64)));

    for bad in [
        "buz/feishu",
        "no-namespace@1.2",
        "ns/id/step@latest",
        "ns/ID@1.2",
        "ns/id/name/extra@1.2",
        "ns/id@1..2",
        "ns/id@^1.2",
        "ns/id@1 - 2",
        &format!("ns/id@sha256:{}", "zz".repeat(32)),
    ] {
        assert!(parse_uses_reference(bad).is_err(), "must reject: {bad}");
    }
}

#[test]
fn uses_invalid_pin_is_a_field_error() {
    let yaml = r#"
name: bad-pin
steps:
  - name: notify
    uses: buz/feishu@latest
"#;
    let r = shared::pipeline_yaml::parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.field == "steps[0].uses" && e.message.contains("unsupported pin")),
        "{:?}",
        r.errors
    );
}

#[test]
fn registry_secret_emits_deprecation_warning() {
    let yaml = r#"
name: legacy registry login
on:
  manual: {}
steps:
  - name: pull
    engine: docker
    image: registry.example.com/app:1.0
    registry_secret: ACR_CREDS
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("registry_secret: deprecated") && w.contains("buz/docker-login")),
        "expected deprecation warning, got: {:?}",
        r.warnings
    );
}

#[test]
fn config_source_emits_deprecation_warning() {
    let yaml = r#"
name: legacy config delivery
on:
  manual: {}
steps:
  - name: deliver
    engine: ssh
    host: web-01
    config:
      source: OSS_CREDS
      key_path: config_tpl/app/v1
      dest: /opt/app
    run: ls "$BUZ_CONFIG_DIR"
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("config.source: deprecated") && w.contains("buz/config-fetch")),
        "expected deprecation warning, got: {:?}",
        r.warnings
    );
}

#[test]
fn plugin_based_config_flow_has_no_deprecation_warning() {
    let yaml = r#"
name: modern config flow
on:
  manual: {}
steps:
  - name: fetch
    uses: buz/config-fetch@1.1.0
    with:
      url: ${secrets.CONFIG_BUNDLE_URL}
      dest: config
  - name: render
    engine: ssh
    host: web-01
    config:
      dest: config
      render:
        - template: env.tpl
          output: .env
    run: ls "$BUZ_CONFIG_DIR"
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(
        !r.warnings.iter().any(|w| w.contains("deprecated")),
        "unexpected deprecation warnings: {:?}",
        r.warnings
    );
}
