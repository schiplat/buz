use std::fmt::Write as _;
use std::path::Path;

use shared::pipeline_yaml::{parse_and_validate, PipelineYaml};
use shared::protocol::StepDef;

pub async fn run(
    pipeline_path: &Path,
    step_index: &Path,
    reviews_path: Option<&Path>,
    require_reviewed: bool,
    offline: bool,
) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(pipeline_path).map_err(|error| {
        anyhow::anyhow!(
            "cannot read pipeline '{}': {error}",
            pipeline_path.display()
        )
    })?;
    let (mut output, images) =
        plan(&raw, step_index, reviews_path, require_reviewed).map_err(anyhow::Error::msg)?;

    if offline {
        for image in &images {
            writeln!(output, "Image availability: {image} — skipped (--offline)").unwrap();
        }
        if images.is_empty() {
            writeln!(output, "Docker image checks: none (no Docker steps)").unwrap();
        }
        writeln!(
            output,
            "Remote Docker/OCI image availability was not checked (--offline).\n"
        )
        .unwrap();
    } else {
        let mut missing = Vec::new();
        for image in &images {
            let result = crate::image_registry::check_image(image).await;
            writeln!(
                output,
                "Image availability: {image} — {}{}",
                result.label(),
                result
                    .detail()
                    .map(|detail| format!(" ({detail})"))
                    .unwrap_or_default()
            )
            .unwrap();
            if result.is_not_found() {
                missing.push(image.clone());
            }
        }
        if images.is_empty() {
            writeln!(output, "Docker image checks: none (no Docker steps)").unwrap();
        }
        if !missing.is_empty() {
            anyhow::bail!(
                "one or more Docker image references were not found: {}\n{output}",
                missing.join(", ")
            );
        }
    }
    print!("{output}");
    Ok(())
}

fn plan(
    raw: &str,
    step_index: &Path,
    reviews_path: Option<&Path>,
    require_reviewed: bool,
) -> Result<(String, Vec<String>), String> {
    let validation = parse_and_validate(raw);
    if !validation.valid {
        return Err(validation
            .errors
            .into_iter()
            .map(|error| format!("{}: {}", error.field, error.message))
            .collect::<Vec<_>>()
            .join("\n"));
    }
    let warnings = validation.warnings;
    let pipeline: PipelineYaml =
        serde_yaml::from_str(raw).map_err(|error| format!("invalid pipeline YAML: {error}"))?;
    let steps: Vec<StepDef> = pipeline.steps.iter().map(StepDef::from).collect();
    let reviewed_hashes = reviews_path
        .map(|path| shared_reviewed_hashes(path))
        .unwrap_or_default();
    let policy = crate::services::step_resolver::ResolutionPolicy {
        reviewed_hashes,
        require_reviewed,
    };
    let resolved = crate::services::step_resolver::resolve_uses_steps(steps, step_index, &policy)?;

    let mut output = String::new();
    let mut images = Vec::new();
    writeln!(output, "Pipeline plan: {}", pipeline.name).unwrap();
    writeln!(output, "Steps: {}", resolved.steps.len()).unwrap();
    writeln!(output, "DRY RUN ONLY: no steps were executed.\n").unwrap();
    for warning in warnings {
        writeln!(output, "Warning: {warning}").unwrap();
    }
    if !output.ends_with("\n\n") {
        writeln!(output).unwrap();
    }

    for (index, step) in resolved.steps.iter().enumerate() {
        writeln!(output, "{}. {}", index + 1, step.name).unwrap();
        let engine = step
            .engine
            .as_deref()
            .filter(|engine| !engine.is_empty())
            .unwrap_or("shell");
        writeln!(output, "   engine: {engine}").unwrap();
        if !step.capabilities.is_empty() {
            writeln!(
                output,
                "   requested capabilities: {}",
                step.capabilities.join(", ")
            )
            .unwrap();
            writeln!(output, "   WARNING: capabilities require separate local Runner administrator authorization.").unwrap();
        }
        if let Some(volumes) = step.volumes.as_ref().filter(|volumes| !volumes.is_empty()) {
            writeln!(output, "   extra host mounts: {}", volumes.join(", ")).unwrap();
        }
        if step.privileged.unwrap_or(false) {
            writeln!(
                output,
                "   WARNING: privileged container requested; this grants broad host-level powers."
            )
            .unwrap();
        }
        if engine == "docker" {
            if let Some(image) = step.image.as_deref() {
                images.push(image.to_string());
                writeln!(output, "   image: {image}").unwrap();
            }
        }
        if let Some(resolution) = resolved
            .resolutions
            .iter()
            .find(|resolution| resolution.step_order == index as i64)
        {
            writeln!(output, "   uses: {}", resolution.uses_ref).unwrap();
            writeln!(
                output,
                "   resolved version: {}",
                resolution.resolved_version
            )
            .unwrap();
            writeln!(output, "   template sha256: {}", resolution.content_sha256).unwrap();
            writeln!(
                output,
                "   reviewed: {}",
                if resolution.reviewed { "yes" } else { "no" }
            )
            .unwrap();
            let mut input_keys = step
                .env
                .as_ref()
                .map(|env| {
                    env.keys()
                        .filter_map(|key| key.strip_prefix("INPUT_"))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            input_keys.sort_unstable();
            if !input_keys.is_empty() {
                writeln!(output, "   inputs: {}", input_keys.join(", ")).unwrap();
            }
        }
    }
    writeln!(output).unwrap();
    writeln!(
        output,
        "This plan does not check Manager secrets/variables, Runner availability, image pull permissions, architecture compatibility, or runtime behavior."
    )

    .unwrap();
    Ok((output, images))
}

fn shared_reviewed_hashes(path: &Path) -> std::collections::HashSet<String> {
    crate::services::step_resolver::load_reviewed_hashes(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::plan;
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
            "name: Missing\nsteps:\n  - name: Missing\n    uses: buz/missing@1.0.0\n",
            index.path(),
            None,
            false
        )
        .unwrap_err()
        .contains("not found in local step index"));
        let missing_input =
            "name: Missing input\nsteps:\n  - name: Upload\n    uses: buz/oss-upload@1.0.0\n";
        assert!(plan(missing_input, index.path(), None, false)
            .unwrap_err()
            .contains("missing required input 'source'"));
        let unknown_input = "name: Unknown input\nsteps:\n  - name: Check\n    uses: buz/http-check@1.0.0\n    with:\n      url: https://example.invalid\n      undeclared: value\n";
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
}
