use std::fmt::Write as _;
use std::path::Path;

use shared::pipeline_yaml::{parse_and_validate, PipelineYaml};
use shared::protocol::StepDef;

pub async fn run(
    pipeline_path: &Path,
    plugin_index: &Path,
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
        plan(&raw, plugin_index, reviews_path, require_reviewed).map_err(anyhow::Error::msg)?;

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

pub fn plan(
    raw: &str,
    plugin_index: &Path,
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
    let policy = crate::services::plugin_resolver::ResolutionPolicy {
        reviewed_hashes,
        require_reviewed,
    };
    let resolved =
        crate::services::plugin_resolver::resolve_uses_steps(steps, plugin_index, &policy)?;

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
    crate::services::plugin_resolver::load_reviewed_hashes(&path.to_string_lossy())
}
