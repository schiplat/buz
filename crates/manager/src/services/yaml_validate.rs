//! DB-backed pipeline YAML validation (structure + referential checks).
//!
//! Structural rules live in `shared::pipeline_yaml::parse_and_validate`.
//! This module adds Manager checks: team slug, secret names, platform variables.
//! Create / update / Validate API must all use this path — failures block writes.
//!
//! **Access:** callers that have not yet authorized the YAML `team:` must use
//! [`validate_pipeline_yaml_for_viewer`] so secret/team probes cannot cross teams.

use shared::error::AppError;
use shared::pipeline_yaml::{
    collect_effective_secret_names, PipelineYaml, YamlValidationError, YamlValidationResult,
};
use shared::rbac::Perm;
use sqlx::{Row, SqlitePool};

use crate::rbac::AuthUser;
use crate::services::plugin_resolver::LocalStepIndex;

/// Validation outcome plus the typed document, so callers that need both
/// (trigger path, scanner) don't pay a second parse. `parsed` is `None`
/// exactly when the YAML does not deserialize.
pub struct ValidatedYaml {
    pub result: YamlValidationResult,
    pub parsed: Option<PipelineYaml>,
}

/// Full validation (structure + referential). Use only after the caller is
/// authorized for the YAML target team, or from the trusted scanner.
///
/// The structural half is content-keyed cached (`services::yaml_cache`);
/// referential checks always run — DB state can change without the file
/// changing.
pub async fn validate_pipeline_yaml(
    pool: &SqlitePool,
    yaml: &str,
) -> Result<YamlValidationResult, AppError> {
    Ok(validate_pipeline_yaml_with_parse(pool, yaml).await?.result)
}

/// [`validate_pipeline_yaml`] retaining the parsed document for reuse.
pub async fn validate_pipeline_yaml_with_parse(
    pool: &SqlitePool,
    yaml: &str,
) -> Result<ValidatedYaml, AppError> {
    let cached = crate::services::yaml_cache::parse_cached(yaml);
    let mut result = cached.structural;
    let Some(parsed) = cached.parsed else {
        return Ok(ValidatedYaml {
            result,
            parsed: None,
        });
    };

    enrich_referential_errors(pool, &parsed, &mut result, ReferentialMode::Full).await?;
    result.valid = result.errors.is_empty();
    Ok(ValidatedYaml {
        result,
        parsed: Some(parsed),
    })
}

/// Validate for an interactive viewer. Without TeamView on the target team,
/// referential checks use a generic team error (no secret-name oracle).
pub async fn validate_pipeline_yaml_for_viewer(
    pool: &SqlitePool,
    yaml: &str,
    auth: &AuthUser,
    local_plugin_dir: &std::path::Path,
) -> Result<YamlValidationResult, AppError> {
    let cached = crate::services::yaml_cache::parse_cached(yaml);
    let mut result = cached.structural;
    let Some(parsed) = cached.parsed else {
        return Ok(result);
    };

    let mode = if auth.is_platform_admin {
        ReferentialMode::Full
    } else {
        ReferentialMode::Viewer { auth }
    };
    enrich_referential_errors(pool, &parsed, &mut result, mode).await?;
    check_uses_availability(pool, &parsed, local_plugin_dir, &mut result).await?;
    result.valid = result.errors.is_empty();
    Ok(result)
}

/// `uses:` availability — every reference must be usable here: an enabled
/// pool entry, or a spec in the local plugin directory. Save resolves the
/// reference anyway (and rejects it when absent); checking during editing
/// tells the author while the typo is still cheap to fix.
///
/// Local lookups only: no template download, no network, and no review-gate
/// verdict (that is a save-time policy, not an availability question).
async fn check_uses_availability(
    pool: &SqlitePool,
    parsed: &PipelineYaml,
    local_dir: &std::path::Path,
    result: &mut YamlValidationResult,
) -> Result<(), AppError> {
    let references: Vec<(usize, String)> = parsed
        .steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| {
            step.uses
                .as_deref()
                .map(str::trim)
                .filter(|uses| !uses.is_empty())
                .map(|uses| (index, uses.to_string()))
        })
        .collect();
    if references.is_empty() {
        return Ok(());
    }

    // Enabled pool rows: the curator's allowlist for remote sources.
    let pool_rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT namespace, step_id, version, sha256 FROM plugin_pool WHERE enabled = 1",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    // Local plugin directory; a missing directory is an empty library, not an
    // error (same semantics as GET /plugins).
    let local_index = LocalStepIndex::new(local_dir.to_path_buf());

    for (index, uses_ref) in references {
        let field = format!("steps.{index}.uses");
        let (reference, pin) = match shared::pipeline_yaml::parse_uses_reference(&uses_ref) {
            Ok(parsed) => parsed,
            Err(error) => {
                result.errors.push(YamlValidationError {
                    field,
                    message: error,
                });
                continue;
            }
        };

        let mut versions: Vec<&str> = pool_rows
            .iter()
            .filter(|(ns, id, _, _)| format!("{ns}/{id}") == reference)
            .map(|(_, _, version, _)| version.as_str())
            .collect();
        let mut hashes: Vec<&str> = pool_rows
            .iter()
            .filter(|(ns, id, _, _)| format!("{ns}/{id}") == reference)
            .map(|(_, _, _, sha)| sha.as_str())
            .collect();
        let local_versions = local_index.available_versions(&reference);
        versions.extend(local_versions.iter().map(|(version, _)| version.as_str()));
        hashes.extend(local_versions.iter().map(|(_, sha)| sha.as_str()));

        if versions.is_empty() {
            result.errors.push(YamlValidationError {
                field,
                message: format!(
                    "plugin '{reference}' is not available here — add its source and enable it in                      Plugin Pool, or place it in the local plugin directory"
                ),
            });
            continue;
        }

        match &pin {
            // Resolved through the source's index at save time; the editor
            // cannot know the branch's current content.
            shared::pipeline_yaml::UsesPin::Branch(_) => {}
            shared::pipeline_yaml::UsesPin::Sha256(hex) => {
                if !hashes
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(hex))
                {
                    result.errors.push(YamlValidationError {
                        field,
                        message: format!(
                            "no plugin '{reference}' with content sha256:{hex} is available here"
                        ),
                    });
                }
            }
            shared::pipeline_yaml::UsesPin::Version(pin) => {
                let matched = versions.iter().any(|version| version_matches(version, pin));
                if !matched {
                    let mut available = versions.clone();
                    available.sort_unstable();
                    available.dedup();
                    result.errors.push(YamlValidationError {
                        field,
                        message: format!(
                            "plugin '{reference}' has no version matching @{pin} here — available: {}",
                            available.join(", ")
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Pin matching mirrors resolution: exact for `@X.Y.Z`, prefix for the
/// `@X` / `@X.Y` major/minor pins.
fn version_matches(version: &str, pin: &str) -> bool {
    version == pin || (pin.split('.').count() < 3 && version.starts_with(&format!("{pin}.")))
}

enum ReferentialMode<'a> {
    Full,
    Viewer { auth: &'a AuthUser },
}

/// Append referential errors (team / secrets / variables). Does not clear structural errors.
async fn enrich_referential_errors(
    pool: &SqlitePool,
    parsed: &PipelineYaml,
    result: &mut YamlValidationResult,
    mode: ReferentialMode<'_>,
) -> Result<(), AppError> {
    let team_slug = parsed
        .team
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default");

    if matches!(&mode, ReferentialMode::Full) {
        check_environment_slugs(pool, parsed, result, false).await?;
    }

    let team_row = sqlx::query("SELECT id FROM teams WHERE slug = ?")
        .bind(team_slug)
        .fetch_optional(pool)
        .await?;

    let Some(team_row) = team_row else {
        match mode {
            ReferentialMode::Full => {
                result.errors.push(YamlValidationError {
                    field: "team".into(),
                    message: format!(
                        "team '{team_slug}' does not exist — create it under Teams, or set team: / owner_group: to an existing slug"
                    ),
                });
            }
            ReferentialMode::Viewer { .. } => {
                // Same message whether missing or inaccessible — avoid team enumeration.
                result.errors.push(YamlValidationError {
                    field: "team".into(),
                    message: "team not found or not accessible".into(),
                });
            }
        }
        // Platform vars are global; Full mode still checks them.
        if matches!(mode, ReferentialMode::Full) {
            check_platform_variables(pool, parsed, result).await?;
        }
        return Ok(());
    };
    let team_id: String = team_row.get(0);

    let viewer_mode = matches!(&mode, ReferentialMode::Viewer { .. });
    if let ReferentialMode::Viewer { auth } = &mode {
        if auth.check_team(&team_id, Perm::TeamView).is_err() {
            result.errors.push(YamlValidationError {
                field: "team".into(),
                message: "team not found or not accessible".into(),
            });
            return Ok(());
        }
    }

    if viewer_mode {
        check_environment_slugs(pool, parsed, result, true).await?;
    }

    let secret_names = collect_effective_secret_names(parsed);
    if !secret_names.is_empty() {
        let placeholders = vec!["?"; secret_names.len()].join(",");
        let query =
            format!("SELECT name FROM secrets WHERE team_id = ? AND name IN ({placeholders})");
        let mut q = sqlx::query_as::<_, (String,)>(&query).bind(&team_id);
        for name in &secret_names {
            q = q.bind(name);
        }
        let found: std::collections::HashSet<String> =
            q.fetch_all(pool).await?.into_iter().map(|r| r.0).collect();
        for name in &secret_names {
            if !found.contains(name) {
                result.errors.push(YamlValidationError {
                    field: "secrets".into(),
                    message: format!(
                        "secret '{name}' not found in team '{team_slug}' — create it under Infrastructure → Secrets (same team)"
                    ),
                });
            }
        }
    }

    check_platform_variables(pool, parsed, result).await?;
    Ok(())
}

async fn check_environment_slugs(
    pool: &SqlitePool,
    parsed: &PipelineYaml,
    result: &mut YamlValidationResult,
    generic_errors: bool,
) -> Result<(), AppError> {
    for slug in &parsed.environments {
        let slug = slug.trim();
        if slug.is_empty() {
            continue;
        }
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM environments WHERE slug = ?)")
                .bind(slug)
                .fetch_one(pool)
                .await?;
        if !exists {
            result.errors.push(YamlValidationError {
                field: "environments".into(),
                message: if generic_errors {
                    "one or more declared environments do not exist".into()
                } else {
                    format!(
                        "environment '{slug}' does not exist — create it under Environments first"
                    )
                },
            });
        }
    }
    Ok(())
}

async fn check_platform_variables(
    pool: &SqlitePool,
    parsed: &PipelineYaml,
    result: &mut YamlValidationResult,
) -> Result<(), AppError> {
    let Some(vars) = parsed.variables.as_ref() else {
        return Ok(());
    };
    let names: Vec<String> = vars
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; names.len()].join(",");
    let query = format!("SELECT name FROM platform_vars WHERE name IN ({placeholders})");
    let mut q = sqlx::query_as::<_, (String,)>(&query);
    for name in &names {
        q = q.bind(name);
    }
    let found: std::collections::HashSet<String> =
        q.fetch_all(pool).await?.into_iter().map(|r| r.0).collect();
    for name in &names {
        if !found.contains(name) {
            result.errors.push(YamlValidationError {
                field: "variables".into(),
                message: format!(
                    "variable '{name}' does not exist — create it under Infrastructure → Variables"
                ),
            });
        }
    }
    Ok(())
}

/// Reject when validation has errors (shared by create / update writers).
pub fn reject_if_invalid(result: &YamlValidationResult) -> Result<(), AppError> {
    if result.valid {
        return Ok(());
    }
    let msg = result
        .errors
        .iter()
        .map(|e| format!("{}: {}", e.field, e.message))
        .collect::<Vec<_>>()
        .join("; ");
    Err(AppError::ValidationFailed(format!(
        "YAML validation failed: {msg}"
    )))
}

/// Resolve `(team_id, slug)` from YAML `team:` / `owner_group:` (omit → `default`).
pub async fn resolve_parsed_team_id(
    pool: &SqlitePool,
    parsed: &PipelineYaml,
) -> Result<(String, String), AppError> {
    let team_slug = parsed
        .team
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default");
    let row = sqlx::query("SELECT id, slug FROM teams WHERE slug = ?")
        .bind(team_slug)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| {
            AppError::ValidationFailed(format!(
                "team '{team_slug}' does not exist — create it under Teams or fix team: / owner_group:"
            ))
        })?;
    Ok((row.get(0), row.get(1)))
}

/// Parse YAML and resolve target team (serde — same as upsert).
pub async fn parse_and_resolve_team(
    pool: &SqlitePool,
    yaml: &str,
) -> Result<(PipelineYaml, String, String), AppError> {
    let parsed: PipelineYaml = serde_yaml::from_str(yaml)
        .map_err(|e| AppError::ValidationFailed(format!("YAML syntax error: {e}")))?;
    let (team_id, slug) = resolve_parsed_team_id(pool, &parsed).await?;
    Ok((parsed, team_id, slug))
}

#[cfg(test)]
#[path = "tests/yaml_validate_tests.rs"]
mod yaml_validate_tests;
