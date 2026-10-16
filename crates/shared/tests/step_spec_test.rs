//! buz-plugin step template spec parsing/validation (marketplace ADR §5.1).

use shared::step_spec::{parse_and_validate_step_spec, validate_step_spec, StepSpec};

const CURRENT: &str = "1.5.6";

fn base_yaml() -> String {
    r#"
id: buz/feishu
name: Feishu notify
version: 1.2.0
description: Send a card message to a Feishu group bot
publisher: official
license: MIT
engine: shell
default_timeout: 60s
inputs:
  webhook:
    type: string
    required: true
    masking: true
    description: Feishu bot webhook URL
  at_all:
    type: boolean
    default: "false"
run: |
  curl -fsSL -X POST "$INPUT_WEBHOOK" -d '{}'
"#
    .to_string()
}

#[test]
fn valid_template_spec_parses_and_validates() {
    let spec = parse_and_validate_step_spec(&base_yaml(), CURRENT).expect("valid spec");
    assert_eq!(spec.namespace(), "buz");
    assert_eq!(spec.step_id(), "feishu");
    assert_eq!(spec.effective_engine(), "shell");
    assert_eq!(spec.input_defs().len(), 2);
    // BTreeMap keys are sorted: at_all < webhook.
    assert_eq!(spec.input_defs()[1].name, "webhook");
    assert!(spec.input_defs()[1].masking.unwrap());
}

#[test]
fn requires_buz_three_part_semantics() {
    let mut yaml = base_yaml();

    yaml = yaml.replacen(
        "engine: shell",
        "requires_buz: \">=1.6.0\"\nengine: shell",
        1,
    );
    let spec: StepSpec = serde_yaml::from_str(&yaml).unwrap();
    let errors = validate_step_spec(&spec, CURRENT).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("too new") && e.contains(">= 1.6")),
        "{errors:?}"
    );

    let satisfied = yaml.replacen(">=1.6.0", ">=1.5.0", 1);
    let spec: StepSpec = serde_yaml::from_str(&satisfied).unwrap();
    assert!(validate_step_spec(&spec, CURRENT).is_ok());

    let exact = base_yaml().replacen("engine: shell", "requires_buz: \"1.5.6\"\nengine: shell", 1);
    let spec: StepSpec = serde_yaml::from_str(&exact).unwrap();
    assert!(validate_step_spec(&spec, CURRENT).is_ok());

    // Unsupported range syntaxes are rejected with the supported forms named.
    for bad in ["<1.6.0", "^1.6.0", "~1.6", "1.6 - 2.0", "1.6"] {
        let y = base_yaml().replacen(
            "engine: shell",
            &format!("requires_buz: \"{bad}\"\nengine: shell"),
            1,
        );
        let spec: StepSpec = serde_yaml::from_str(&y).unwrap();
        let errors = validate_step_spec(&spec, CURRENT).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("only '>=X.Y.Z' and exact 'X.Y.Z'")),
            "{bad}: {errors:?}"
        );
    }
}

#[test]
fn wasm_runtime_and_unknown_fields_rejected() {
    let wasm = base_yaml().replacen("engine: shell", "runtime: wasm\nengine: shell", 1);
    let errors =
        validate_step_spec(&serde_yaml::from_str::<StepSpec>(&wasm).unwrap(), CURRENT).unwrap_err();
    assert!(errors.iter().any(|e| e.contains("Phase 3")), "{errors:?}");

    assert!(serde_yaml::from_str::<StepSpec>("id: a/b\nversion: 1.0.0\noops: 1").is_err());
}

#[test]
fn docker_requires_image_and_run_bans_secret_refs() {
    let docker = base_yaml().replacen("engine: shell", "engine: docker", 1);
    let errors = validate_step_spec(&serde_yaml::from_str::<StepSpec>(&docker).unwrap(), CURRENT)
        .unwrap_err();
    assert!(
        errors.iter().any(|e| e.contains("image: is required")),
        "{errors:?}"
    );

    let leak = base_yaml().replacen(
        "curl -fsSL -X POST \"$INPUT_WEBHOOK\" -d '{}'",
        "echo ${secrets.FEISHU_WEBHOOK}",
        1,
    );
    let errors =
        validate_step_spec(&serde_yaml::from_str::<StepSpec>(&leak).unwrap(), CURRENT).unwrap_err();
    assert!(
        errors.iter().any(|e| e.contains("${secrets.")),
        "{errors:?}"
    );
}

#[test]
fn malformed_ids_versions_and_inputs_rejected() {
    let bad = base_yaml()
        .replacen("id: buz/feishu", "id: notify", 1)
        .replacen("version: 1.2.0", "version: 1.2", 1)
        .replacen("at_all:\n    type: boolean", "at_all:\n    type: banana", 1);
    let errors = parse_and_validate_step_spec(&bad, CURRENT).unwrap_err();
    assert!(errors.iter().any(|e| e.starts_with("id:")), "{errors:?}");
    assert!(
        errors.iter().any(|e| e.starts_with("version:")),
        "{errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.starts_with("inputs.at_all:")),
        "{errors:?}"
    );
}

#[test]
fn community_publisher_requires_attribution() {
    // Omitted publisher defaults to community → author + source_repo required.
    let community = base_yaml().replacen("publisher: official\n", "", 1);
    let errors = validate_step_spec(
        &serde_yaml::from_str::<StepSpec>(&community).unwrap(),
        CURRENT,
    )
    .unwrap_err();
    assert!(
        errors.iter().any(|e| e.starts_with("author:")),
        "{errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.starts_with("source_repo:")),
        "{errors:?}"
    );

    let complete = base_yaml()
        .replacen("publisher: official", "publisher: community", 1)
        .replacen("id: buz/feishu", "id: alice/telegram", 1)
        .replacen(
            "engine: shell",
            "author: someone\nsource_repo: https://github.com/someone/steps\nengine: shell",
            1,
        );
    let spec: StepSpec = serde_yaml::from_str(&complete).unwrap();
    assert!(validate_step_spec(&spec, CURRENT).is_ok());

    // Explicit official keeps attribution optional — base_yaml IS official.
    let spec: StepSpec = serde_yaml::from_str(&base_yaml()).unwrap();
    assert!(validate_step_spec(&spec, CURRENT).is_ok());

    let bad = base_yaml().replacen("publisher: official", "publisher: verified", 1);
    let errors =
        validate_step_spec(&serde_yaml::from_str::<StepSpec>(&bad).unwrap(), CURRENT).unwrap_err();
    assert!(
        errors.iter().any(|e| e.starts_with("publisher:")),
        "{errors:?}"
    );
}

#[test]
fn buz_namespace_is_reserved_for_official_templates() {
    // Community template using the reserved `buz` namespace → rejected.
    let community = base_yaml()
        .replacen("publisher: official", "publisher: community", 1)
        .replacen(
            "engine: shell",
            "author: someone\nsource_repo: https://github.com/someone/steps\nengine: shell",
            1,
        );
    let errors = validate_step_spec(
        &serde_yaml::from_str::<StepSpec>(&community).unwrap(),
        CURRENT,
    )
    .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("namespace 'buz' is reserved")),
        "{errors:?}"
    );

    // Official template outside the `buz` namespace → rejected.
    let misplaced = base_yaml().replacen("id: buz/feishu", "id: tools/feishu", 1);
    let errors = validate_step_spec(
        &serde_yaml::from_str::<StepSpec>(&misplaced).unwrap(),
        CURRENT,
    )
    .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("must live in the 'buz' namespace")),
        "{errors:?}"
    );
}

#[test]
fn id_grammar_is_two_segments_for_all_templates() {
    // Official: buz/<name>; category is optional browsing metadata.
    let spec: StepSpec = serde_yaml::from_str(&base_yaml()).unwrap();
    assert!(validate_step_spec(&spec, CURRENT).is_ok());

    // Community: <handle>/<name> — same grammar, own namespace.
    let community = base_yaml()
        .replacen("publisher: official", "publisher: community", 1)
        .replacen("id: buz/feishu", "id: alice/telegram", 1)
        .replacen(
            "engine: shell",
            "author: someone\nsource_repo: https://github.com/someone/steps\nengine: shell",
            1,
        );
    let spec: StepSpec = serde_yaml::from_str(&community).unwrap();
    assert!(validate_step_spec(&spec, CURRENT).is_ok());

    // The old three-segment form is invalid.
    let three = base_yaml().replacen("id: buz/feishu", "id: buz/notify/feishu", 1);
    let errors = validate_step_spec(&serde_yaml::from_str::<StepSpec>(&three).unwrap(), CURRENT)
        .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("expected '<namespace>/<name>'")),
        "{errors:?}"
    );
}

#[test]
fn host_inputs_are_restricted_for_marketplace_templates() {
    // ssh templates execute on the pipeline step's host — the target is never
    // chosen through a template input. (engine: ssh itself is allowed; see
    // ssh_engine_template_is_valid_without_image.)
    let host = base_yaml().replacen("type: boolean", "type: host", 1);
    let errors =
        validate_step_spec(&serde_yaml::from_str::<StepSpec>(&host).unwrap(), CURRENT).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("type: host is not supported")),
        "{errors:?}"
    );
}

#[test]
fn input_names_colliding_after_env_key_normalization_are_rejected() {
    let collision = base_yaml().replacen(
        "  at_all:\n    type: boolean\n    default: \"false\"",
        "  api-key:\n    type: string\n  apiKey:\n    type: string\n  at_all:\n    type: boolean\n    default: \"false\"",
        1,
    );
    let errors = validate_step_spec(
        &serde_yaml::from_str::<StepSpec>(&collision).unwrap(),
        CURRENT,
    )
    .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.contains("normalizes to INPUT_API_KEY")),
        "{errors:?}"
    );
}

#[test]
fn input_defaults_obey_masking_types_options_and_patterns() {
    let masked_default = base_yaml().replacen(
        "    masking: true\n    description:",
        "    masking: true\n    default: \"literal-secret\"\n    description:",
        1,
    );
    let errors = validate_step_spec(
        &serde_yaml::from_str::<StepSpec>(&masked_default).unwrap(),
        CURRENT,
    )
    .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("masked inputs must not declare a literal default")),
        "{errors:?}"
    );

    let number = base_yaml().replacen(
        "  at_all:\n    type: boolean\n    default: \"false\"",
        "  retries:\n    type: number\n    default: \"many\"\n  at_all:\n    type: boolean\n    default: \"false\"",
        1,
    );
    let errors = validate_step_spec(&serde_yaml::from_str::<StepSpec>(&number).unwrap(), CURRENT)
        .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("inputs.retries.default: must be an integer")),
        "{errors:?}"
    );

    let choice = base_yaml().replacen(
        "  at_all:\n    type: boolean\n    default: \"false\"",
        "  mode:\n    type: choice\n    options: [safe, fast]\n    default: \"unknown\"\n  at_all:\n    type: boolean\n    default: \"false\"",
        1,
    );
    let errors = validate_step_spec(&serde_yaml::from_str::<StepSpec>(&choice).unwrap(), CURRENT)
        .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("inputs.mode.default: must match one of the declared options")),
        "{errors:?}"
    );

    let pattern = base_yaml().replacen(
        "  at_all:\n    type: boolean\n    default: \"false\"",
        "  tag:\n    type: string\n    default: \"not-valid\"\n    pattern: ^[a-z]+$\n  at_all:\n    type: boolean\n    default: \"false\"",
        1,
    );
    let errors = validate_step_spec(
        &serde_yaml::from_str::<StepSpec>(&pattern).unwrap(),
        CURRENT,
    )
    .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("inputs.tag.default: does not match pattern")),
        "{errors:?}"
    );

    let malformed_pattern = base_yaml().replacen(
        "    type: boolean\n    default: \"false\"",
        "    type: boolean\n    default: \"false\"\n    pattern: (",
        1,
    );
    let errors = validate_step_spec(
        &serde_yaml::from_str::<StepSpec>(&malformed_pattern).unwrap(),
        CURRENT,
    )
    .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("inputs.at_all: invalid pattern")),
        "{errors:?}"
    );
}

#[test]
fn credential_like_inputs_must_be_masked() {
    let leaky = base_yaml().replacen(
        "  at_all:",
        "  bot_token:\n    type: string\n    default: \"x\"\n  at_all:",
        1,
    );
    let errors = validate_step_spec(&serde_yaml::from_str::<StepSpec>(&leaky).unwrap(), CURRENT)
        .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.starts_with("inputs.bot_token:") && e.contains("masking: true")),
        "{errors:?}"
    );

    // The existing webhook input is masked → base template stays valid.
    let spec: StepSpec = serde_yaml::from_str(&base_yaml()).unwrap();
    assert!(validate_step_spec(&spec, CURRENT).is_ok());
}

#[test]
fn ssh_engine_template_is_valid_without_image() {
    // ssh templates execute on the pipeline-declared target host; no image
    // field is required. Community (unmanaged) and governed official deploy
    // templates both rely on this — see the marketplace governance document.
    let yaml = base_yaml().replacen("engine: shell", "engine: ssh", 1);
    let spec = parse_and_validate_step_spec(&yaml, CURRENT).expect("valid ssh spec");
    assert_eq!(spec.effective_engine(), "ssh");
    assert!(validate_step_spec(&spec, CURRENT).is_ok());
}

#[test]
fn engines_declares_extra_call_site_engines() {
    let mut yaml = base_yaml();
    yaml.push_str("engines: [shell, ssh]\n");
    let spec = parse_and_validate_step_spec(&yaml, CURRENT).expect("valid spec");
    assert_eq!(spec.declared_engines(), vec!["shell", "ssh"]);
    assert!(spec.engine_override_ok("ssh"));
    assert!(spec.engine_override_ok("SSH ")); // normalized
    assert!(!spec.engine_override_ok("docker"));
}

#[test]
fn engines_default_is_engine_only() {
    let spec = parse_and_validate_step_spec(&base_yaml(), CURRENT).expect("valid spec");
    assert_eq!(spec.declared_engines(), vec!["shell"]);
    assert!(spec.engine_override_ok("shell"));
    assert!(!spec.engine_override_ok("ssh"));
}

#[test]
fn invalid_declared_engine_is_rejected() {
    let mut yaml = base_yaml();
    yaml.push_str("engines: [shell, wasm]\n");
    let err = parse_and_validate_step_spec(&yaml, CURRENT).unwrap_err();
    assert!(
        err.iter()
            .any(|e| e.contains("is not valid for a step template")),
        "{err:?}"
    );
}

#[test]
fn docker_declared_without_image_is_rejected() {
    let mut yaml = base_yaml();
    yaml.push_str("engines: [shell, docker]\n");
    let err = parse_and_validate_step_spec(&yaml, CURRENT).unwrap_err();
    assert!(
        err.iter().any(|e| e.contains("image: is required")),
        "{err:?}"
    );
}
