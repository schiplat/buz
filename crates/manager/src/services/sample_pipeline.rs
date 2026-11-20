//! Built-in sample pipeline for fresh deployments.
//!
//! A brand-new Manager seeds one `Hello Buz` pipeline into the `default` team
//! so the Pipelines page is not empty on first login. The sample is ordinary
//! data: users may edit or delete it, and the `manager_meta` marker prevents
//! startup from recreating it. Existing installations are left untouched
//! (`skipped-existing`); `buz-manager seed-sample` can add it explicitly.

use crate::config::Config;
use crate::db;
use crate::services::pipeline_scanner;
use crate::services::plugin_resolver;
use crate::store::exec::DbPool;

/// `manager_meta` key recording the outcome of the one-time sample seed.
pub const META_KEY: &str = "sample_pipeline_v1";
/// Stored YAML filename (also the DB `yaml_path`).
pub const FILENAME: &str = "hello-buz.yml";
/// Pipeline name used for the duplicate guard.
pub const SAMPLE_NAME: &str = "Hello Buz";
/// Embedded so every build (Docker included) carries the same definition.
pub const SAMPLE_YAML: &str = include_str!("../../assets/hello-buz.yml");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedOutcome {
    Seeded,
    /// A marker already exists — the one-time decision was taken.
    AlreadyMarked,
    /// Existing installation (users or pipelines present): never seed.
    SkippedExisting,
    /// A YAML definition with this filename already exists.
    SkippedFile,
    /// A pipeline with the sample name already exists.
    SkippedPipeline,
}

impl SeedOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Seeded => "seeded",
            Self::AlreadyMarked => "already-marked",
            Self::SkippedExisting => "skipped-existing",
            Self::SkippedFile => "skipped-file-exists",
            Self::SkippedPipeline => "skipped-pipeline-exists",
        }
    }
}

/// Startup path: seed only when the database looks fresh (no users yet).
/// Idempotent via [`META_KEY`], and upgrades are marked as skipped so later
/// boots do not re-run the checks.
pub async fn seed_if_fresh(db: &DbPool, config: &Config) -> anyhow::Result<SeedOutcome> {
    if db::manager_meta_get(db, META_KEY).await?.is_some() {
        return Ok(SeedOutcome::AlreadyMarked);
    }
    if crate::store::auth::count_users(db).await? > 0 {
        db::manager_meta_set(db, META_KEY, SeedOutcome::SkippedExisting.as_str()).await?;
        return Ok(SeedOutcome::SkippedExisting);
    }
    if crate::store::pipelines::count_all(db).await? > 0 {
        db::manager_meta_set(db, META_KEY, SeedOutcome::SkippedExisting.as_str()).await?;
        return Ok(SeedOutcome::SkippedExisting);
    }
    seed_inner(db, config).await
}

/// Explicit operator request (`buz-manager seed-sample`): ignore the marker so
/// a deleted sample can be restored, but never create a duplicate.
pub async fn seed_forced(db: &DbPool, config: &Config) -> anyhow::Result<SeedOutcome> {
    seed_inner(db, config).await
}

async fn seed_inner(db: &DbPool, config: &Config) -> anyhow::Result<SeedOutcome> {
    if pipeline_scanner::find_yaml_pipeline_row(db, FILENAME)
        .await?
        .is_some()
    {
        db::manager_meta_set(db, META_KEY, SeedOutcome::SkippedFile.as_str()).await?;
        return Ok(SeedOutcome::SkippedFile);
    }
    if crate::store::pipelines::count_by_name(db, SAMPLE_NAME).await? > 0 {
        db::manager_meta_set(db, META_KEY, SeedOutcome::SkippedPipeline.as_str()).await?;
        return Ok(SeedOutcome::SkippedPipeline);
    }

    let step_index_dir = plugin_resolver::effective_local_dir(
        &config.storage.data_dir,
        &config.plugin_index.local_dir,
    )?;
    let result = pipeline_scanner::write_and_upsert_yaml(
        db,
        FILENAME,
        SAMPLE_YAML,
        &step_index_dir,
        None,
        config.plugin_index.sync_timeout_seconds,
        None,
        true,
    )
    .await?;

    db::manager_meta_set(db, META_KEY, SeedOutcome::Seeded.as_str()).await?;
    tracing::info!(
        pipeline_id = %result.pipeline_id,
        name = SAMPLE_NAME,
        "sample pipeline seeded"
    );
    Ok(SeedOutcome::Seeded)
}
