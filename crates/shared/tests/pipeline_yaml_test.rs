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
    // `uses: shell` previously satisfied "run or uses", while unknown `ru:` was ignored.
    let yaml = r#"
name: test
steps:
  - name: Render env
    uses: shell
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
    uses: shell
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
    uses: shell
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
    uses: shell
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
    uses: shel
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(!r.valid);
    assert!(
        r.errors
            .iter()
            .any(|e| e.field.contains("uses") && e.message.contains("shel")),
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
      deploy_token:
        description: One-time token
        required: true
        masking: true
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
    let inputs = parsed
        .trigger_config
        .as_ref()
        .and_then(|c| c.inputs.as_ref())
        .expect("inputs");
    let token = inputs.iter().find(|i| i.name == "deploy_token").unwrap();
    assert_eq!(token.masking, Some(true));
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
fn hosts_strategy_chain_default_and_parallel_opt_in() {
    let omitted = r#"
name: Multi
steps:
  - name: deploy
    uses: ssh
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
    uses: ssh
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
    uses: ssh
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
    uses: ssh
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
    uses: shell
    hosts_strategy: parallel
    run: true
"#;
    let r2 = parse_and_validate(on_shell);
    assert!(!r2.valid);
    assert!(
        r2.errors
            .iter()
            .any(|e| e.message.contains("only valid for uses: ssh")),
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
    uses: docker
    image: alpine:3.20
    run: echo hi
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    let parsed = r.parsed.unwrap();
    assert_eq!(parsed.steps[0].uses.as_deref(), Some("docker"));
    assert_eq!(parsed.steps[0].image.as_deref(), Some("alpine:3.20"));
}

#[test]
fn docker_step_missing_image_rejected() {
    let yaml = r#"
name: Docker Bad
steps:
  - name: hello
    uses: docker
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
    uses: docker
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
    uses: docker
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
    uses: shell
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
    uses: docker
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
    uses: shell
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
    uses: ssh
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
    uses: shell
    config:
      source: OSS_DEMO
      key_path: config_tpl/demo/v1
      dest: /tmp/cfg
    run: ls "$BUZ_CONFIG_DIR"
"#;
    let r = parse_and_validate(yaml);
    assert!(r.valid, "Errors: {:?}", r.errors);
    assert!(
        r.warnings.is_empty(),
        "unexpected warnings: {:?}",
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
    uses: shell
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
    uses: shell
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
    uses: ssh
    host: host-b
    config:
      dest: /opt/manager
      render:
        template: env.example
        output: .env
        mode: "0600"
        on_missing: fail
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
}

#[test]
fn config_render_list_normalizes_to_vec() {
    let yaml = r#"
name: multi-render
steps:
  - name: render
    uses: shell
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
    uses: shell
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
    uses: shell
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
    uses: shell
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
    uses: shell
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
    uses: shell
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
    uses: shell
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
    uses: shell
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
    uses: docker
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
    uses: shell
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
    uses: shell
    registry_secret: ${secrets.ACR_PERSONAL}
    run: docker push example.com/app:1
  - name: deliver
    uses: ssh
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
