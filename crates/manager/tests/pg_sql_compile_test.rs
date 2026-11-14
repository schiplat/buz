//! Postgres compile gate for the store's statement corpus.
//!
//! The SQLite validator (`store_sql_validation_test`) proves every store
//! statement compiles against the SQLite schema. This is its Postgres
//! analogue: every `Sql::Shared` statement is rewritten (placeholder
//! renumbering) and `PREPARE`d against the PG baseline, and every explicit
//! `Sql::Dual` Postgres body is `PREPARE`d verbatim.
//!
//! It also checks the dual-body bind invariant: both bodies of a `Dual`
//! statement must accept the same number of binds, because the executor hands
//! one `binds` slice to either backend.
//!
//! Runs only when `BUZ_TEST_PG_URL` points at a reachable Postgres 14+; each
//! run creates and drops its own schema. Without the variable the test is a
//! no-op, so default `cargo test` stays SQLite-only.

#![cfg(feature = "postgres")]

use std::path::{Path, PathBuf};

use sqlx::postgres::PgPoolOptions;

/// SELECT fragments spliced in by `format!`; must match `src/store`
/// (kept in sync with the SQLite validator).
const FRAGMENTS: &[(&str, &str)] = &[
    (
        "{CONNECTION_COLUMNS}",
        "id, name, api_base_url, token_ciphertext, fetch_owners, fetch_interval_minutes, last_fetch_at, updated_at",
    ),
    (
        "{REPO_COLUMNS}",
        "id, connection_id, owner, repo, updated_at, created_at",
    ),
    (
        "{ENV_PROTECTION_SELECT}",
        "protection_enabled, min_trigger_role, min_approve_role, require_approvals, forbid_self_approve, approval_timeout_seconds",
    ),
];

const KEYWORDS: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE", "WITH"];

struct Literal {
    label: String,
    text: String,
}

/// Extract every plain string literal from Rust source, skipping comments and
/// honouring `\` escapes (same scanner as the SQLite validator).
fn string_literals(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    let mut line_comment = false;
    let mut block_comment = false;
    while i < chars.len() {
        let c = chars[i];
        if line_comment {
            if c == '\n' {
                line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_comment {
            if c == '*' && chars.get(i + 1) == Some(&'/') {
                block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            line_comment = true;
            i += 2;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            block_comment = true;
            i += 2;
            continue;
        }
        if c == '"' {
            i += 1;
            let mut text = String::new();
            while i < chars.len() {
                let ch = chars[i];
                if ch == '\\' && i + 1 < chars.len() {
                    let next = chars[i + 1];
                    text.push(match next {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '0' => '\0',
                        other => other,
                    });
                    i += 2;
                    continue;
                }
                if ch == '"' {
                    i += 1;
                    break;
                }
                text.push(ch);
                i += 1;
            }
            out.push(text);
            continue;
        }
        i += 1;
    }
    out
}

fn is_statement(text: &str) -> bool {
    let head = text.trim_start();
    KEYWORDS.iter().any(|keyword| {
        head.starts_with(keyword)
            && head[keyword.len()..]
                .chars()
                .next()
                .is_none_or(|c| c.is_whitespace() || c == '(')
    })
}

fn resolve_fragments(text: &str) -> Option<String> {
    let mut resolved = text.to_string();
    for (name, value) in FRAGMENTS {
        resolved = resolved.replace(name, value);
    }
    if resolved.contains('{') || resolved.contains('}') {
        return None;
    }
    Some(resolved)
}

fn store_files() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("store dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        // Same exclusions as the SQLite validator: the rewriter's grammar
        // tokens, exec/sql test fixtures, and this module's own tests.
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_none_or(|name| {
                    !name.starts_with("._")
                        && name != "dialect.rs"
                        && name != "exec.rs"
                        && name != "sql.rs"
                })
        })
        .collect();
    files.sort();
    files
}

/// All `Sql::Dual { sqlite: "…", pg: "…" }` pairs in one source file.
fn dual_bodies(source: &str) -> Vec<(String, String)> {
    manager::store::sql::dual_bodies(source)
}

/// Mark the characters that are SQL code: outside single-quoted literals and
/// outside `--` / `/* */` comments. The store's Postgres bodies carry
/// apostrophes inside comments (`SQLite's …`), so comment awareness is not
/// optional here.
fn code_mask(sql: &str) -> (Vec<char>, Vec<bool>) {
    let chars: Vec<char> = sql.chars().collect();
    let mut mask = vec![false; chars.len()];
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            continue;
        }
        if c == '\'' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        mask[i] = true;
        i += 1;
    }
    (chars, mask)
}

/// `?` placeholders in SQL code (outside literals and comments).
fn sqlite_placeholder_count(sql: &str) -> usize {
    let (chars, mask) = code_mask(sql);
    chars
        .iter()
        .zip(&mask)
        .filter(|(c, code)| **code && **c == '?')
        .count()
}

/// The `$n` placeholders of a Postgres body, outside literals and comments.
fn postgres_placeholders(sql: &str) -> Vec<usize> {
    let (chars, mask) = code_mask(sql);
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if mask[i] && chars[i] == '$' {
            let mut j = i + 1;
            while j < chars.len() && mask[j] && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 {
                out.push(chars[i + 1..j].iter().collect::<String>().parse().unwrap());
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Replace every `$n` in SQL code with `NULL`, for the retry pass when
/// Postgres cannot infer a parameter's type.
fn placeholders_to_null(sql: &str) -> String {
    let (chars, mask) = code_mask(sql);
    let mut out = String::with_capacity(sql.len());
    let mut i = 0usize;
    while i < chars.len() {
        if mask[i] && chars[i] == '$' {
            let mut j = i + 1;
            while j < chars.len() && mask[j] && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 {
                out.push_str("NULL");
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

async fn setup() -> Option<(String, sqlx::PgPool, sqlx::PgPool)> {
    let url = std::env::var("BUZ_TEST_PG_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())?;
    let schema = format!("buz_compile_{}", uuid::Uuid::new_v4().simple());

    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect admin pool");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create schema");

    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .expect("parse pg url")
        .options([("search_path", schema.as_str())]);
    let pg = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .expect("connect schema pool");
    manager::db::migrate_pg::run_pg_migrations(&pg)
        .await
        .expect("apply pg baseline");

    Some((schema, pg, admin))
}

fn is_param_inference_error(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(db)
            if matches!(
                db.code().as_deref(),
                // indeterminate_datatype / ambiguous_parameter / ambiguous_function
                Some("42P18") | Some("42P08") | Some("42725")
            )
    )
}

#[tokio::test]
async fn postgres_prepares_every_store_statement() {
    let Some((schema, pg, admin)) = setup().await else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping Postgres compile gate");
        return;
    };

    // Collect the Dual pairs first, then everything else from the literal
    // scan. A literal that is a dual body is classified by its pair, not by
    // the generic corpus.
    let mut dual_pairs: Vec<(String, String)> = Vec::new();
    let mut literals: Vec<Literal> = Vec::new();
    for path in store_files() {
        let label = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let source = std::fs::read_to_string(&path).expect("read store file");
        dual_pairs.extend(dual_bodies(&source));
        for text in string_literals(&source) {
            literals.push(Literal {
                label: label.clone(),
                text,
            });
        }
    }

    let dual_pg: Vec<&str> = dual_pairs.iter().map(|(_, pg)| pg.as_str()).collect();
    let dual_sqlite: Vec<&str> = dual_pairs
        .iter()
        .map(|(sqlite, _)| sqlite.as_str())
        .collect();

    // Bind invariant: both bodies take the same binds in the same order.
    let mut arity_failures = Vec::new();
    for (sqlite, pg) in &dual_pairs {
        let sqlite_binds = sqlite_placeholder_count(sqlite);
        let pg_params = postgres_placeholders(pg);
        let pg_binds = pg_params.iter().copied().max().unwrap_or(0);
        let mut dense = pg_params.clone();
        dense.sort_unstable();
        dense.dedup();
        if sqlite_binds != pg_binds || dense != (1..=pg_binds).collect::<Vec<_>>() {
            arity_failures.push(format!(
                "sqlite has {sqlite_binds} bind(s), postgres uses {pg_params:?}\n  pg: {pg}"
            ));
        }
    }

    let mut statements: Vec<(String, String)> = Vec::new(); // (label, SQL)
    let mut skipped_dynamic = 0usize;
    for literal in &literals {
        let text = &literal.text;
        if dual_pg.iter().any(|dual| dual == text) || dual_sqlite.iter().any(|dual| dual == text) {
            continue;
        }
        if !is_statement(text) {
            continue;
        }
        let Some(text) = resolve_fragments(text) else {
            skipped_dynamic += 1;
            continue;
        };
        if text.contains("$1") {
            // A PG-only body outside `Sql::Dual` — still worth compiling.
            statements.push((literal.label.clone(), text));
        } else {
            statements.push((
                literal.label.clone(),
                manager::store::dialect::sqlite_to_postgres(&text),
            ));
        }
    }

    // De-duplicate: several call sites can hold the same statement.
    statements.sort_by(|a, b| a.1.cmp(&b.1));
    statements.dedup_by(|a, b| a.1 == b.1);

    let mut failures = Vec::new();
    let mut uninferred = Vec::new();
    for (index, (label, sql)) in statements.iter().enumerate() {
        let prepared = format!("PREPARE buz_stmt_{index} AS {sql}");
        match sqlx::raw_sql(&prepared).execute(&pg).await {
            Ok(_) => {}
            Err(error) if is_param_inference_error(&error) => {
                let retry = format!("PREPARE buz_stmt_{index} AS {}", placeholders_to_null(sql));
                match sqlx::raw_sql(&retry).execute(&pg).await {
                    Ok(_) => uninferred.push(format!("{label}: {sql}")),
                    Err(retry_error) => failures.push(format!(
                        "{label}: {error} (NULL retry: {retry_error})\n  -> {sql}"
                    )),
                }
            }
            Err(error) => failures.push(format!("{label}: {error}\n  -> {sql}")),
        }
    }

    println!(
        "prepared {} statement(s); {} dual pair(s); {} dynamic fragment(s) skipped; {} type-uninferred (retried as NULL)",
        statements.len(),
        dual_pairs.len(),
        skipped_dynamic,
        uninferred.len()
    );
    for entry in &uninferred {
        println!("  type-uninferred: {entry}");
    }

    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
    let _ = pg.close().await;

    assert!(
        statements.len() >= 40,
        "extractor found only {} statement(s); did the store directory move?",
        statements.len()
    );
    assert!(
        !dual_pairs.is_empty(),
        "no Sql::Dual pairs found — the classifier needs updating"
    );
    assert!(
        arity_failures.is_empty(),
        "dual statements disagree on bind count:\n\n{}",
        arity_failures.join("\n\n")
    );
    assert!(
        failures.is_empty(),
        "{} store statement(s) do not compile on Postgres:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
