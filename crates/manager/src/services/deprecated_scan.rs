//! Deprecated-key scan — Phase 3 precondition ① (registry-auth-dual-track.md
//! §6 / object-store-config-source.md §4).
//!
//! Read-only report of pipelines and secrets still carrying retired data in
//! the database (`tasks.registry_secret`, `tasks.config_source` /
//! `tasks.config_key_path` columns, secret kinds `registry` /
//! `aliyun_oss_auth`). The kernel no longer consumes these columns — they are
//! report-only until the next release drops them. Pipeline YAML files
//! with the retired keys (`registry_secret`, `config.source` /
//! `config.key_path`) are rejected outright by the strict parser
//! (`deny_unknown_fields`) and show up under `files_unreadable` — that is the
//! migration signal. Modifies nothing.

use std::io::Write;
use std::path::Path;

use sqlx::{Row, SqlitePool};

/// One affected artifact and the deprecated markers it carries.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeprecatedFinding {
    /// Human-readable location: `pipeline / step "name"` or a secret name.
    pub location: String,
    /// Deprecated markers found (`registry_secret`, `config.source`,
    /// `config.key_path`, or `kind=registry` / `kind=aliyun_oss_auth`).
    pub keys: Vec<String>,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct ScanReport {
    /// Configured YAML scan dir (`""` when unset/unwritable).
    pub yaml_scan_dir: String,
    /// Live pipelines with deprecated keys in their task rows (covers both
    /// form-authored and YAML-imported pipelines).
    pub task_findings: Vec<DeprecatedFinding>,
    /// YAML files on disk still carrying deprecated keys. Always empty since
    /// the strict parser rejects those files outright — the findings live in
    /// `files_unreadable` instead. Kept for report-shape stability.
    pub file_findings: Vec<DeprecatedFinding>,
    /// Secrets whose kind is one of the retired kinds (migration needed).
    pub secret_findings: Vec<DeprecatedFinding>,
    /// YAML files that failed to parse (reported, not fatal). Files using the
    /// retired `registry_secret` / `config.source` / `config.key_path` keys
    /// land here via the strict-parse rejection.
    pub files_unreadable: Vec<String>,
}

impl ScanReport {
    pub fn is_empty(&self) -> bool {
        self.task_findings.is_empty()
            && self.file_findings.is_empty()
            && self.secret_findings.is_empty()
            && self.files_unreadable.is_empty()
    }

    pub fn print(&self, out: &mut dyn Write) -> std::io::Result<()> {
        writeln!(
            out,
            "Buz deprecated-key scan (pre-retirement migration report; read-only)"
        )?;

        let dir = if self.yaml_scan_dir.is_empty() {
            "(not configured)".to_string()
        } else {
            self.yaml_scan_dir.clone()
        };

        writeln!(out, "\nPipelines (Manager DB):")?;
        if self.task_findings.is_empty() {
            writeln!(out, "  no deprecated keys in task rows")?;
        }
        for f in &self.task_findings {
            writeln!(out, "  - {} → {}", f.location, f.keys.join(", "))?;
        }

        writeln!(out, "\nPipeline YAML files ({dir}):")?;
        if self.file_findings.is_empty() && self.files_unreadable.is_empty() {
            writeln!(out, "  no files with deprecated keys")?;
        }
        for f in &self.file_findings {
            writeln!(out, "  - {} → {}", f.location, f.keys.join(", "))?;
        }
        for unreadable in &self.files_unreadable {
            writeln!(out, "  - {unreadable}")?;
        }

        writeln!(out, "\nSecrets with retired kinds:")?;
        if self.secret_findings.is_empty() {
            writeln!(out, "  none")?;
        }
        for f in &self.secret_findings {
            writeln!(out, "  - {} → {}", f.location, f.keys.join(", "))?;
        }

        writeln!(
            out,
            "\nSummary: {} pipeline(s), {} file(s), {} secret(s) affected.",
            self.task_findings.len(),
            self.file_findings.len(),
            self.secret_findings.len(),
        )?;
        writeln!(
            out,
            "Nothing was modified. Migration: registry_secret → buz/docker-login step + two generic secrets (REGISTRY_USERNAME / REGISTRY_PASSWORD); config.source/key_path → buz/config-fetch (or buz/oss-download) step + render-only config block; kind=registry / kind=aliyun_oss_auth secrets → split into generic secrets or delete. Pipeline YAML files with the retired keys are rejected outright by the strict parser (see the unreadable-file list)."
        )?;
        Ok(())
    }
}

/// Scan a database and optional YAML directory. Used by the CLI and tests;
/// does not run migrations — callers open the pool the same way the server
/// does (which runs them), so the legacy columns are present.
pub async fn scan_pool(pool: &SqlitePool, yaml_scan_dir: Option<&Path>) -> ScanReport {
    let mut report = ScanReport::default();

    // 1. Live task rows — covers form-authored and YAML-imported pipelines.
    // The kernel no longer reads these columns; they are report-only until the
    // next release drops them.
    let rows = sqlx::query(
        "SELECT p.name AS pipeline_name, t.name AS step_name,
                t.registry_secret, t.config_source, t.config_key_path
         FROM tasks t
         JOIN pipelines p ON p.id = t.pipeline_id
         WHERE t.registry_secret != '' OR t.config_source != '' OR t.config_key_path != ''
         ORDER BY p.name, t.step_order",
    )
    .fetch_all(pool)
    .await;
    match rows {
        Ok(rows) => {
            for row in rows {
                let mut keys = Vec::new();
                if !row.get::<String, _>("registry_secret").is_empty() {
                    keys.push("registry_secret".to_string());
                }
                if !row.get::<String, _>("config_source").is_empty() {
                    keys.push("config.source".to_string());
                }
                if !row.get::<String, _>("config_key_path").is_empty() {
                    keys.push("config.key_path".to_string());
                }
                report.task_findings.push(DeprecatedFinding {
                    location: format!(
                        "{} / step \"{}\"",
                        row.get::<String, _>("pipeline_name"),
                        row.get::<String, _>("step_name"),
                    ),
                    keys,
                });
            }
        }
        Err(e) => report
            .files_unreadable
            .push(format!("(db) task scan failed: {e}")),
    }

    // 2. Secrets on the two retired kinds. The columns are added by
    // run_migrations; a missing column means a pre-kind database — report and
    // continue rather than fail the whole scan.
    let rows = sqlx::query(
        "SELECT name, kind FROM secrets WHERE kind IN ('registry', 'aliyun_oss_auth') ORDER BY name",
    )
    .fetch_all(pool)
    .await;
    match rows {
        Ok(rows) => {
            for row in rows {
                report.secret_findings.push(DeprecatedFinding {
                    location: row.get::<String, _>("name"),
                    keys: vec![format!("kind={}", row.get::<String, _>("kind"))],
                });
            }
        }
        Err(e) => report
            .files_unreadable
            .push(format!("(db) secret scan failed: {e}")),
    }

    // 3. YAML files on disk. The retired keys (`registry_secret`,
    // `config.source`, `config.key_path`) are unknown fields now, so any file
    // still using them fails the strict parse and is reported as unreadable —
    // that rejection IS the migration signal. Parseable files carry no
    // deprecated keys by construction, so no file findings are produced here.
    if let Some(dir) = yaml_scan_dir {
        report.yaml_scan_dir = dir.display().to_string();
        if dir.is_dir() {
            match shared::yaml_files::discover_yaml_files(dir) {
                Ok(files) => {
                    for file in files {
                        scan_yaml_file(&file, &mut report);
                    }
                }
                Err(e) => report
                    .files_unreadable
                    .push(format!("{}: {e}", dir.display())),
            }
        }
    }

    report
}

/// Parse one YAML file; unparseable/unreadable files are reported so operators
/// can fix (migrate) them. Files carrying the retired keys are rejected by the
/// strict parser and surface here with the unknown-field error.
fn scan_yaml_file(file: &Path, report: &mut ScanReport) {
    let display = file.display().to_string();
    let raw = match std::fs::read_to_string(file) {
        Ok(raw) => raw,
        Err(e) => {
            report
                .files_unreadable
                .push(format!("{display}: unreadable ({e})"));
            return;
        }
    };
    let result = shared::pipeline_yaml::parse_and_validate(&raw);
    if result.parsed.is_none() {
        let first = result
            .errors
            .first()
            .map(|e| e.message.clone())
            .unwrap_or_else(|| "invalid pipeline YAML".to_string());
        report
            .files_unreadable
            .push(format!("{display}: not parsed ({first})"));
    }
}

/// CLI entry: load config, open + migrate the database, scan, print.
pub async fn run(config_path: &str) -> anyhow::Result<()> {
    let config = crate::config::load(Some(config_path))?;
    let pool = crate::db::init(&config).await?;
    crate::db::run_migrations(&pool, &config.auth.jwt_secret).await?;

    let yaml_dir = {
        let dir = &config.pipeline.yaml_scan_dir;
        let trimmed = dir.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(trimmed))
        }
    };
    let report = scan_pool(&pool, yaml_dir.as_deref()).await;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    report.print(&mut out)?;
    Ok(())
}
