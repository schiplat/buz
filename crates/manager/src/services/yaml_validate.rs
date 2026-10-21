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
    result.valid = result.errors.is_empty();
    Ok(result)
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
