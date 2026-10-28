use manager::db::*;
use sqlx::sqlite::SqlitePoolOptions;

/// A database created before the plugin rename must come out of
/// `run_migrations` with the new table and settings-key names — the data,
/// not just the schema, has to survive the rename.
#[tokio::test]
async fn legacy_step_tables_and_settings_keys_are_renamed() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE step_sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL, \
             auto_enable INTEGER NOT NULL DEFAULT 0, enabled INTEGER NOT NULL DEFAULT 1, \
             last_synced_at TEXT, last_sync_error TEXT, created_by TEXT, \
             created_at TEXT NOT NULL DEFAULT (datetime('now')))",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO step_sources (id, name, url) VALUES ('src-legacy', 'legacy', 'https://x/index.json')")
            .execute(&pool)
            .await
            .unwrap();
    sqlx::query(
        "CREATE TABLE step_pool (namespace TEXT NOT NULL, step_id TEXT NOT NULL, \
             version TEXT NOT NULL, sha256 TEXT NOT NULL, source_id TEXT NOT NULL, \
             file_path TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 0, \
             reviewed INTEGER NOT NULL DEFAULT 0, added_by TEXT, \
             added_at TEXT NOT NULL DEFAULT (datetime('now')), \
             UNIQUE(namespace, step_id, version))",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE platform_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, \
             updated_by TEXT, updated_at TEXT NOT NULL DEFAULT (datetime('now')))",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO platform_settings (key, value) VALUES ('step_review_required', 'true')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO platform_settings (key, value) \
             VALUES ('step_official_source_dismissed', 'true')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO step_pool (namespace, step_id, version, sha256, source_id, file_path) \
             VALUES ('buz', 'archive', '1.0.0', '{}', 'src-legacy', 'buz/archive/1.0.0.yaml')",
    )
    .execute(&pool)
    .await
    .unwrap();

    run_migrations(&pool, "test-secret-0123456789-abcdefghijklmnop")
        .await
        .expect("migrations run on a legacy database");

    // Migrations also seed the factory source, so assert the legacy row
    // itself survived rather than a total count.
    let legacy: Option<String> =
        sqlx::query_scalar("SELECT name FROM plugin_sources WHERE id = 'src-legacy'")
            .fetch_optional(&pool)
            .await
            .expect("plugin_sources exists");
    assert_eq!(
        legacy.as_deref(),
        Some("legacy"),
        "source row survived the rename"
    );

    let review_key: Option<String> = sqlx::query_scalar(
        "SELECT value FROM platform_settings WHERE key = 'plugin_review_required'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(review_key.as_deref(), Some("true"), "settings key renamed");

    let stale: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM platform_settings WHERE key LIKE 'step_%'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stale, 0, "no legacy keys left behind");

    // The pool's own column followed the vocabulary: the legacy row is
    // readable under plugin_id (RENAME COLUMN also rewrote the UNIQUE
    // constraint and the lookup index that referenced step_id).
    let pooled: Option<String> = sqlx::query_scalar(
        "SELECT version FROM plugin_pool \
             WHERE namespace = 'buz' AND plugin_id = 'archive'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(pooled.as_deref(), Some("1.0.0"), "pool column renamed");
    let old_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('plugin_pool') WHERE name = 'step_id'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(old_column, 0, "no legacy column left behind");
}
