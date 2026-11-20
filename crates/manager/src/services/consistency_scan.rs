//! Team-binding consistency CLI (`buz-manager check-data`) — read-only.
//!
//! The same report is logged as a single warning at startup when it is not
//! clean; this command prints the per-surface counts so an operator can decide
//! what to fix. It never writes.

use std::io::Write;

use crate::store::consistency::ConsistencyReport;

pub async fn run(config_path: &str) -> anyhow::Result<()> {
    let config = crate::config::load(Some(config_path))?;
    let db = crate::db::init(&config).await?;
    if let Some(sqlite) = db.as_sqlite() {
        crate::db::run_migrations(sqlite, &config.auth.jwt_secret).await?;
    }

    let report = crate::store::consistency::check(&db).await?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    print(&report, &mut out)?;
    Ok(())
}

fn print(report: &ConsistencyReport, out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(out, "Buz team-binding consistency check (read-only)")?;
    writeln!(out, "\nPipelines:")?;
    writeln!(out, "  missing team_id: {}", report.pipelines_missing_team)?;
    writeln!(out, "  unknown team_id: {}", report.pipelines_unknown_team)?;
    writeln!(out, "\nSecrets:")?;
    writeln!(out, "  missing team_id: {}", report.secrets_missing_team)?;
    writeln!(out, "  unknown team_id: {}", report.secrets_unknown_team)?;
    writeln!(out, "\nHosts:")?;
    writeln!(out, "  unknown team_id: {}", report.hosts_unknown_team)?;
    writeln!(out, "\nPipeline secret bindings:")?;
    writeln!(
        out,
        "  spanning two teams (ignored at runtime): {}",
        report.cross_team_secret_bindings
    )?;

    if report.is_clean() {
        writeln!(out, "\nclean — no legacy binding rows found")?;
    } else {
        writeln!(
            out,
            "\nissues found. The write-path guards refuse new rows of these shapes;"
        )?;
        writeln!(
            out,
            "fix legacy rows by moving the resource to an existing team, deleting it, or unbinding it."
        )?;
    }
    Ok(())
}
