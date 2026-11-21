//! Instance branding: the administrator-uploaded custom logo.
//!
//! A single-row table. **An absent row means "use the built-in mark"** — the
//! default look is never touched by this module, and deleting the row is the
//! reset path. The image lives as base64 text so one statement body serves
//! both backends; the API caps its size before it ever reaches storage.

use crate::store::exec::{self, DbPool, SqlValue};

use shared::error::AppError;

/// The saved custom logo (`id = 1` singleton).
#[derive(Debug, Clone)]
pub struct BrandingRow {
    pub logo_mime: String,
    pub logo_base64: String,
    pub logo_bytes: i64,
    pub updated_by: String,
    pub updated_at: String,
}

/// The singleton row, when an administrator has uploaded a logo.
pub async fn get(pool: &DbPool) -> Result<Option<BrandingRow>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT logo_mime, logo_base64, logo_bytes, updated_by, updated_at
         FROM branding WHERE id = 1",
        &[],
    )
    .await?
    .map(|r| BrandingRow {
        logo_mime: r.text_of("logo_mime"),
        logo_base64: r.text_of("logo_base64"),
        logo_bytes: r.i64_of("logo_bytes"),
        updated_by: r.text_of("updated_by"),
        updated_at: r.text_of("updated_at"),
    }))
}

/// Save (or replace) the singleton row.
pub async fn upsert(
    pool: &DbPool,
    logo_mime: &str,
    logo_base64: &str,
    logo_bytes: i64,
    updated_by: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO branding (id, logo_mime, logo_base64, logo_bytes, updated_by)
         VALUES (1, ?, ?, ?, ?)
         ON CONFLICT (id) DO UPDATE SET
            logo_mime = excluded.logo_mime,
            logo_base64 = excluded.logo_base64,
            logo_bytes = excluded.logo_bytes,
            updated_by = excluded.updated_by,
            updated_at = datetime('now')",
        &[
            logo_mime.into(),
            logo_base64.into(),
            SqlValue::Int(logo_bytes),
            updated_by.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Drop the row: the built-in mark applies again.
pub async fn delete(pool: &DbPool) -> Result<bool, AppError> {
    let affected = exec::execute(pool, "DELETE FROM branding WHERE id = 1", &[]).await?;
    Ok(affected != 0)
}
