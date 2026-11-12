//! Throwaway: dump the fully migrated template schema so the PG baseline DDL
//! can be derived from the real end state (not hand-walked).

mod common;

use common::migrated_pool;
use sqlx::Row;

#[tokio::test]
async fn dump_schema() {
    let pool = migrated_pool(2).await;
    let rows = sqlx::query(
        "SELECT type, name, sql FROM sqlite_master \
         WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY rowid",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let mut out = String::new();
    for r in &rows {
        let kind: String = r.get(0);
        let name: String = r.get(1);
        let sql: String = r.get(2);
        out.push_str(&format!("-- {kind} {name}\n{sql};\n\n"));
    }
    std::fs::write("/tmp/buz_schema_dump.sql", &out).unwrap();
    println!("dumped {} objects to /tmp/buz_schema_dump.sql", rows.len());
}
