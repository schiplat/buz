//! `pipeline export` / `pipeline import` — pipeline definitions as files.
//!
//! The database is the authority. `export` writes a read-only snapshot of every
//! stored definition (intended to be committed to a git repository for audit
//! and review); `import` reads such a directory back into the database. Import
//! is per file and never deletes: a definition missing from the directory is
//! left untouched.
//!
//! Neither command is a runtime path — the Manager never watches a directory.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::store::exec::DbPool;

use crate::services::pipeline_scanner::{
    filename_from_pipeline_name, validate_yaml_filename, write_and_upsert_yaml,
};

#[derive(Debug, Default)]
pub struct ExportReport {
    pub written: usize,
    pub unchanged: usize,
    pub skipped: Vec<String>,
    pub dir: PathBuf,
}

pub fn print_export(report: &ExportReport, out: &mut dyn std::io::Write) -> std::io::Result<()> {
    writeln!(
        out,
        "exported {} definition(s) to {} ({} unchanged)",
        report.written,
        report.dir.display(),
        report.unchanged
    )?;
    for skipped in &report.skipped {
        writeln!(out, "  skipped: {skipped}")?;
    }
    Ok(())
}

/// Write every stored definition to `dir` as `<origin>.yml`.
///
/// Files are replaced only when their bytes differ, so exporting into a git
/// working tree produces a stable diff. Rows without a stored definition are
/// listed as skipped (their YAML was never backfilled).
pub async fn export_definitions(pool: &DbPool, dir: &Path) -> anyhow::Result<ExportReport> {
    std::fs::create_dir_all(dir)?;
    let rows = crate::store::pipelines::yaml_definition_rows(pool)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut report = ExportReport {
        dir: dir.to_path_buf(),
        ..Default::default()
    };
    for (id, name, yaml_path, source) in rows {
        if source.is_empty() {
            report
                .skipped
                .push(format!("{name} ({id}): no stored definition"));
            continue;
        }
        let filename = yaml_path
            .as_deref()
            .and_then(crate::services::pipeline_scanner::filename_from_yaml_path)
            .unwrap_or_else(|| filename_from_pipeline_name(&name));
        let target = dir.join(&filename);
        if std::fs::read_to_string(&target).ok().as_deref() == Some(source.as_str()) {
            report.unchanged += 1;
            continue;
        }
        // Temp + rename: a failed export never leaves a half-written snapshot.
        let tmp = dir.join(format!(".{filename}.tmp"));
        std::fs::write(&tmp, &source)?;
        std::fs::rename(&tmp, &target)?;
        report.written += 1;
    }
    Ok(report)
}

#[derive(Debug, Default)]
pub struct ImportReport {
    pub stored: usize,
    pub unchanged: usize,
    pub failed: Vec<String>,
    pub dir: PathBuf,
    pub dry_run: bool,
}

pub fn print_import(report: &ImportReport, out: &mut dyn std::io::Write) -> std::io::Result<()> {
    let mode = if report.dry_run { " (dry run)" } else { "" };
    writeln!(
        out,
        "imported {} definition(s) from {}{mode} ({} unchanged)",
        report.stored,
        report.dir.display(),
        report.unchanged
    )?;
    for failed in &report.failed {
        writeln!(out, "  failed: {failed}")?;
    }
    Ok(())
}

/// Read `*.yml` / `*.yaml` files from `dir` and store each as a definition.
///
/// Files are matched by basename: importing `deploy.yml` updates the pipeline
/// whose origin is `deploy.yml` or creates one. Validation, `uses:` resolution
/// and the task projection run exactly as for an API save. A file that fails is
/// reported and does not stop the remaining files.
pub async fn import_definitions(
    pool: &DbPool,
    dir: &Path,
    step_index_dir: &Path,
    sync_timeout_secs: u64,
    dry_run: bool,
) -> anyhow::Result<ImportReport> {
    let mut report = ImportReport {
        dir: dir.to_path_buf(),
        dry_run,
        ..Default::default()
    };
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(filename) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if validate_yaml_filename(filename).is_err() {
            continue;
        }
        names.insert(filename.to_string());
    }

    for filename in names {
        let path = dir.join(&filename);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                report.failed.push(format!("{filename}: {error}"));
                continue;
            }
        };
        if dry_run {
            match crate::services::yaml_validate::validate_pipeline_yaml(pool, &content).await {
                Ok(validation) if validation.valid => report.stored += 1,
                Ok(validation) => {
                    let messages: Vec<String> = validation
                        .errors
                        .iter()
                        .map(|e| format!("{}: {}", e.field, e.message))
                        .collect();
                    report
                        .failed
                        .push(format!("{filename}: {}", messages.join("; ")));
                }
                Err(error) => report.failed.push(format!("{filename}: {error}")),
            }
            continue;
        }
        match write_and_upsert_yaml(
            pool,
            &filename,
            &content,
            step_index_dir,
            None,
            sync_timeout_secs,
            None,
            false,
        )
        .await
        {
            Ok(_) => report.stored += 1,
            Err(error) => report.failed.push(format!("{filename}: {error}")),
        }
    }
    Ok(report)
}
