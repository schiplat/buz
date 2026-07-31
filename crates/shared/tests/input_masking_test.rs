use std::collections::HashMap;

use shared::protocol::{masking_input_names, redact_input_map, PipelineInputDef};

#[test]
fn masking_input_names_filters_true_only() {
    let defs = vec![
        PipelineInputDef {
            name: "version".into(),
            description: None,
            required: Some(true),
            default: None,
            masking: None,
        },
        PipelineInputDef {
            name: "token".into(),
            description: None,
            required: None,
            default: None,
            masking: Some(true),
        },
        PipelineInputDef {
            name: "nope".into(),
            description: None,
            required: None,
            default: None,
            masking: Some(false),
        },
    ];
    assert_eq!(masking_input_names(&defs), vec!["token".to_string()]);
}

#[test]
fn redact_input_map_masks_listed_keys_only() {
    let mut inputs = HashMap::new();
    inputs.insert("version".into(), "1.2.3".into());
    inputs.insert("token".into(), "super-secret".into());
    let redacted = redact_input_map(&inputs, &["token".into()]);
    assert_eq!(redacted.get("version").map(String::as_str), Some("1.2.3"));
    assert_eq!(redacted.get("token").map(String::as_str), Some("***"));
}
