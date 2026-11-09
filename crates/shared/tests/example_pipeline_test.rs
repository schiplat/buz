//! Example pipelines under `example/pipelines/` must pass schema validation
//! — they are the copy-paste surface users see first.

use shared::pipeline_yaml::parse_and_validate;

#[test]
fn needs_dag_demo_validates() {
    let raw = include_str!("../../../example/pipelines/needs-dag-demo.yml");
    let result = parse_and_validate(raw);
    assert!(result.valid, "{:?}", result.errors);
}
