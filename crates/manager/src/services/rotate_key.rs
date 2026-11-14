//! `buz-manager rotate-jwt-key` — rotate the JWT secret and re-encrypt every
//! stored value in place.
//!
//! The JWT secret is a root key (signs tokens AND derives the AES-256-GCM
//! key that encrypts secrets at rest — see `manager::crypto`). Changing it
//! without re-encrypting makes every stored value unreadable; this command
//! performs the missing half of that operation: decrypt with the CURRENT
//! secret, re-encrypt with the NEW one, update the rows in a single
//! transaction.
//!
//! Safety model:
//! - **Plan is the default.** Without `--apply` the command validates and
//!   reports only — zero writes. `--apply` re-runs the same validations and
//!   only then mutates.
//! - A `VACUUM INTO` snapshot is mandatory before the transaction.
//! - The whole rotation is one transaction; any failure rolls everything
//!   back.
//! - After the update, every new blob is decrypted back under the new key
//!   (in-transaction roundtrip) before committing.
//!
//! Encrypted surfaces covered here (keep in sync with `crypto::encrypt`
//! call sites): `secrets.value`, `github_connections.token_ciphertext`,
//! `pipeline_git_hooks.secret_ciphertext`,
//! `environment_secret_overrides.value`, `hosts.credential_data`, and
//! `buzenc:v1:`-prefixed values inside `jobs.trigger_params`.

use serde::Serialize;
use sqlx::{Row, SqlitePool};

use crate::config;
use crate::crypto;

/// CLI entry: resolve config (same chain as the service → the CURRENT
/// secret), open the database, and run plan or apply.
pub async fn run(
    config_path: &str,
    db_path_override: Option<&str>,
    do_apply: bool,
    new_secret_file: Option<&str>,
) -> anyhow::Result<()> {
    let cfg = config::load_unchecked(Some(config_path))?;
    let old_secret = cfg.auth.jwt_secret;
    if old_secret.trim().is_empty() {
        anyhow::bail!(
            "no jwt_secret resolved from {config_path} / BUZ_JWT_SECRET — the current key must \
             come from the same config the Manager runs with"
        );
    }

    let db_path = db_path_override.unwrap_or(&cfg.database.path);

    // Generate the new secret up front so plan validates the REAL pair.
    let generated;
    let new_secret = match new_secret_file {
        Some(f) => {
            let s = std::fs::read_to_string(f)
                .map_err(|e| anyhow::anyhow!("cannot read --new-secret-file {f}: {e}"))?
                .trim()
                .to_string();
            generated = s;
            &generated
        }
        None => {
            generated = format!("{:064x}", rand::random::<u128>() as u128)
                + &format!("{:064x}", rand::random::<u128>());
            &generated
        }
    };

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{}?mode=rwc", db_path))
        .await
        .map_err(|e| anyhow::anyhow!("cannot open {db_path}: {e}"))?;

    let report = plan(&pool, &old_secret, new_secret)
        .await
        .map_err(anyhow::Error::msg)?;
    let total = report.total_values();
    println!(
        "[plan] current key fingerprint: {}",
        report.old_key_fingerprint
    );
    for e in &report.entries {
        println!(
            "[plan]   {}.{:<28} {:>3} row(s), plaintext {} bytes",
            e.table, e.column, e.rows, e.plaintext_bytes
        );
    }
    println!(
        "[plan]   jobs.trigger_params       {:>3} job(s) with buzenc:v1 values",
        report.trigger_param_jobs
    );

    if total == 0 {
        println!("[plan] nothing encrypted to rotate — update the config secret and restart.");
        return Ok(());
    }

    if !do_apply {
        println!(
            "[plan] {total} value(s) validated against the current key. Nothing written.\n\
             Re-run with --apply to perform the rotation, then start the Manager with the \
             new secret ({}).",
            new_secret_file
                .map(|f| f.to_string())
                .unwrap_or_else(|| "stored beside the database".into())
        );
        pool.close().await;
        return Ok(());
    }

    // Persist the generated secret BEFORE mutating the database: if the
    // rotation succeeds but the secret were lost, every value would be
    // unreadable.
    let secret_out = new_secret_file.map(str::to_string).unwrap_or_else(|| {
        let dir = std::path::Path::new(db_path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let p = dir.join(".jwt_secret");
        p.to_string_lossy().into_owned()
    });
    if new_secret_file.is_none() {
        if let Some(parent) = std::path::Path::new(&secret_out).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&secret_out, new_secret)
            .map_err(|e| anyhow::anyhow!("cannot write {secret_out}: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&secret_out, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| anyhow::anyhow!("chmod {secret_out}: {e}"))?;
        }
        println!("[apply] new secret stored: {secret_out} (0600)");
    }

    let report = apply(&pool, db_path, &old_secret, new_secret)
        .await
        .map_err(anyhow::Error::msg)?;
    println!(
        "[apply] rotated {} value(s) OK — start the Manager with BUZ_JWT_SECRET \
         from {secret_out} (or the same value via config).",
        report.total_values()
    );
    pool.close().await;
    Ok(())
}

/// Columns storing `crypto::encrypt` blobs (table, column).
/// Encrypted-at-rest columns with the row-key expression that identifies a
/// row in both SQLite and Postgres (`||` string concat is dialect-neutral).
/// `environment_secret_overrides` has a composite primary key, hence the
/// concat instead of a bare `id` column.
pub const ENCRYPTED_COLUMNS: &[(&str, &str, &str)] = &[
    ("secrets", "value", "id"),
    ("github_connections", "token_ciphertext", "id"),
    ("pipeline_git_hooks", "secret_ciphertext", "id"),
    (
        "environment_secret_overrides",
        "value",
        "environment_id || ':' || secret_id",
    ),
    ("hosts", "credential_data", "id"),
];

const TRIGGER_PARAM_PREFIX: &str = "buzenc:v1:";

/// One validated inventory line of the plan.
#[derive(Debug, Clone, Serialize)]
pub struct PlanEntry {
    pub table: String,
    pub column: String,
    pub rows: i64,
    /// Sum of plaintext sizes — a rough "what lives here" indicator without
    /// ever printing a value.
    pub plaintext_bytes: i64,
}

/// Result of the read-only plan phase.
#[derive(Debug, Clone, Serialize)]
pub struct PlanReport {
    pub entries: Vec<PlanEntry>,
    /// Jobs whose `trigger_params` carry `buzenc:v1:` values.
    pub trigger_param_jobs: i64,
    /// The fingerprint (first 8 hex of SHA-256) of the resolved current key.
    pub old_key_fingerprint: String,
}

impl PlanReport {
    pub fn total_values(&self) -> i64 {
        self.entries.iter().map(|e| e.rows).sum::<i64>() + self.trigger_param_jobs
    }
}

/// Validate that `old_secret` decrypts every stored blob and that
/// `new_secret` is acceptable. Pure read — safe to run on a live instance.
pub async fn plan(
    pool: &SqlitePool,
    old_secret: &str,
    new_secret: &str,
) -> Result<PlanReport, String> {
    if old_secret == new_secret {
        return Err("the new secret equals the current one — nothing to rotate".into());
    }
    crate::config::validate_jwt_secret(new_secret)
        .map_err(|e| format!("new secret rejected: {e}"))?;

    let mut entries = Vec::new();
    for (table, column, key_expr) in ENCRYPTED_COLUMNS {
        let mut rows = 0i64;
        let mut bytes = 0i64;
        let query = format!(
            "SELECT {key_expr} AS row_key, {column} FROM {table} WHERE {column} IS NOT NULL AND {column} != ''"
        );
        for row in sqlx::query(&query).fetch_all(pool).await.map_err(|e| {
            format!(
                "cannot read {table}.{column}: {e} (is the schema current? run the manager once)"
            )
        })? {
            let row_key: String = row.get(0);
            let blob: String = row.get(1);
            let plain = crypto::decrypt(&blob, old_secret).map_err(|_| {
                format!(
                    "decrypt failed on {table}.{column} ({row_key}) — the old secret does \
                     not match this database"
                )
            })?;
            rows += 1;
            bytes += plain.len() as i64;
        }
        if rows > 0 {
            entries.push(PlanEntry {
                table: (*table).to_string(),
                column: (*column).to_string(),
                rows,
                plaintext_bytes: bytes,
            });
        }
    }

    let trigger_jobs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE trigger_params LIKE '%buzenc:v1:%'")
            .fetch_one(pool)
            .await
            .map_err(|e| format!("cannot count jobs.trigger_params: {e}"))?;

    Ok(PlanReport {
        entries,
        trigger_param_jobs: trigger_jobs,
        old_key_fingerprint: crypto::key_fingerprint(old_secret),
    })
}

/// Perform the rotation: snapshot, re-encrypt every value in one
/// transaction, verify, commit. The caller must have stopped the manager.
pub async fn apply(
    pool: &SqlitePool,
    db_path: &str,
    old_secret: &str,
    new_secret: &str,
) -> Result<PlanReport, String> {
    // Mandatory snapshot before any write.
    let backup = format!("{db_path}.pre-rotation");
    sqlx::query("VACUUM INTO ?")
        .bind(&backup)
        .execute(pool)
        .await
        .map_err(|e| format!("snapshot to {backup} failed: {e}"))?;

    let report = plan(pool, old_secret, new_secret).await?;

    let mut tx = pool.begin().await.map_err(|e| format!("begin: {e}"))?;

    for (table, column, key_expr) in ENCRYPTED_COLUMNS {
        let query = format!(
            "SELECT {key_expr} AS row_key, {column} FROM {table} WHERE {column} IS NOT NULL AND {column} != ''"
        );
        for row in sqlx::query(&query)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| format!("read {table}.{column} failed: {e}"))?
        {
            let row_key: String = row.get(0);
            let blob: String = row.get(1);
            let plain = crypto::decrypt(&blob, old_secret).map_err(|_| {
                format!(
                    "decrypt failed on {table}.{column} ({row_key}) — aborting, nothing written"
                )
            })?;
            let updated = crypto::encrypt(&plain, new_secret)
                .map_err(|e| format!("re-encrypt {table}.{column} failed: {e}"))?;
            sqlx::query(&format!("UPDATE {table} SET {column}=? WHERE {key_expr}=?"))
                .bind(&updated)
                .bind(&row_key)
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("update {table}.{column} failed: {e}"))?;
        }
    }

    // jobs.trigger_params: JSON values carrying `buzenc:v1:` strings.
    let job_rows =
        sqlx::query("SELECT id, trigger_params FROM jobs WHERE trigger_params LIKE '%buzenc:v1:%'")
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| format!("read jobs.trigger_params failed: {e}"))?;
    for row in job_rows {
        let id: String = row.get(0);
        let tp: String = row.get(1);
        let updated = rewalk_trigger_params(&tp, old_secret, new_secret)
            .map_err(|e| format!("jobs[{id}] trigger_params: {e}"))?;
        sqlx::query("UPDATE jobs SET trigger_params=? WHERE id=?")
            .bind(&updated)
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("update jobs[{id}] failed: {e}"))?;
    }

    // In-transaction roundtrip: every re-encrypted blob must open under the
    // new key before we commit.
    for (table, column, key_expr) in ENCRYPTED_COLUMNS {
        let query = format!(
            "SELECT {key_expr} AS row_key, {column} FROM {table} WHERE {column} IS NOT NULL AND {column} != ''"
        );
        for row in sqlx::query(&query)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| format!("roundtrip read {table}.{column} failed: {e}"))?
        {
            let row_key: String = row.get(0);
            let blob: String = row.get(1);
            if crypto::decrypt(&blob, new_secret).is_err() {
                return Err(format!(
                    "roundtrip failed on {table}.{column} ({row_key}) — rolled back, nothing written"
                ));
            }
        }
    }

    tx.commit().await.map_err(|e| format!("commit: {e}"))?;
    println!("snapshot: {backup}");
    Ok(report)
}

fn rewalk_trigger_params(tp: &str, old_secret: &str, new_secret: &str) -> Result<String, String> {
    let mut obj: serde_json::Value =
        serde_json::from_str(tp).map_err(|e| format!("not JSON: {e}"))?;
    walk(&mut obj, old_secret, new_secret)?;
    serde_json::to_string(&obj).map_err(|e| format!("serialize: {e}"))
}

fn walk(v: &mut serde_json::Value, old_secret: &str, new_secret: &str) -> Result<(), String> {
    match v {
        serde_json::Value::Object(map) => {
            for value in map.values_mut() {
                walk(value, old_secret, new_secret)?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for item in items {
                walk(item, old_secret, new_secret)?;
            }
            Ok(())
        }
        serde_json::Value::String(s) if s.starts_with(TRIGGER_PARAM_PREFIX) => {
            let plain =
                crypto::decrypt(&s[TRIGGER_PARAM_PREFIX.len()..], old_secret).map_err(|_| {
                    "decrypt failed — the old secret does not match this job's params".to_string()
                })?;
            *s = format!(
                "{TRIGGER_PARAM_PREFIX}{}",
                crypto::encrypt(&plain, new_secret)?
            );
            Ok(())
        }
        _ => Ok(()),
    }
}
