//! buz-plugin step template spec parsing/validation (marketplace ADR §5.1).

use shared::step_spec::{parse_and_validate_step_spec, validate_step_spec, StepSpec};

const CURRENT: &str = "1.5.6";

fn base_yaml() -> String {
    r#"
id: buz/notify/feishu
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
        .replacen("id: buz/notify/feishu", "id: notify", 1)
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
        .replacen("id: buz/notify/feishu", "id: alice/notify/telegram", 1)
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
    let misplaced = base_yaml().replacen("id: buz/notify/feishu", "id: tools/notify/feishu", 1);
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
fn id_grammar_is_three_segments_for_all_templates() {
    // Official: buz/<category>/<name>.
    let spec: StepSpec = serde_yaml::from_str(&base_yaml()).unwrap();
    assert!(validate_step_spec(&spec, CURRENT).is_ok());

    // Community: <handle>/<category>/<name> — same grammar, own namespace.
    let community = base_yaml()
        .replacen("publisher: official", "publisher: community", 1)
        .replacen("id: buz/notify/feishu", "id: alice/notify/telegram", 1)
        .replacen(
            "engine: shell",
            "author: someone\nsource_repo: https://github.com/someone/steps\nengine: shell",
            1,
        );
    let spec: StepSpec = serde_yaml::from_str(&community).unwrap();
    assert!(validate_step_spec(&spec, CURRENT).is_ok());

    // Two segments (ns/name) no longer parse as valid ids.
    let two = base_yaml().replacen("id: buz/notify/feishu", "id: buz/feishu", 1);
    let errors =
        validate_step_spec(&serde_yaml::from_str::<StepSpec>(&two).unwrap(), CURRENT).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("expected '<namespace>/<category>/<name>'")),
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
