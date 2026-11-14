//! Dialect rewriter invariants, checked against **every** statement in
//! `store::` — the same corpus `store_sql_validation_test` compiles.
//!
//! The rewriter is the PG port's load-bearing wall: if it mangles a statement
//! (drops a literal, misnumbers a placeholder, breaks an operator), the failure
//! surfaces on an endpoint nobody traced. These tests hold the wall:
//! - string literal contents survive byte-for-byte;
//! - no bare `?` remains outside a literal;
//! - `$N` numbering is strictly 1..=N in source order;
//! - portable syntax (datetime shims, ON CONFLICT upserts) passes through
//!   untouched — the rewriter must not "improve" it.

use manager::store::dialect::sqlite_to_postgres;

/// Mirror of the SQL-validator's literal scanner (string-aware, escape-aware):
/// returns the statement's SQL text with string literal *contents* masked, so
/// placeholder/keyword assertions cannot be fooled by text inside literals.
fn masked(sql: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    let mut in_literal = false;
    while i < chars.len() {
        let c = chars[i];
        if in_literal {
            if c == '\'' {
                if chars.get(i + 1) == Some(&'\'') {
                    i += 1;
                } else {
                    in_literal = false;
                }
            }
            i += 1;
            continue;
        }
        if c == '\'' {
            in_literal = true;
            out.push('\'');
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Every statement in `store::`, extracted the same way the schema validator
/// extracts them.
fn all_statements() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("src/store");
    for entry in walk_paths(&src) {
        let text = std::fs::read_to_string(&entry).unwrap_or_default();
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '"' {
                let mut j = i + 1;
                let mut lit = String::new();
                while j < chars.len() && chars[j] != '"' {
                    if chars[j] == '\\' && j + 1 < chars.len() {
                        j += 1;
                        lit.push(chars[j]);
                    } else {
                        lit.push(chars[j]);
                    }
                    j += 1;
                }
                if is_statement(&lit) {
                    // Hand-written Postgres bodies already use `$n` and jsonb
                    // operators; they are not SQLite-dialect input for the
                    // rewriter (see `postgres_statements_are_declared`).
                    if !lit.contains("$1") {
                        out.push((entry.display().to_string(), lit));
                    }
                }
                i = j + 1;
                continue;
            }
            i += 1;
        }
    }
    out
}

fn is_statement(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with("SELECT ")
        || t.starts_with("SELECT\n")
        || t.starts_with("INSERT ")
        || t.starts_with("UPDATE ")
        || t.starts_with("DELETE ")
        || t.starts_with("WITH ")
}

fn walk_paths(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk_paths(&p));
            } else if p.extension().is_some_and(|x| x == "rs")
                && p.file_name().and_then(|n| n.to_str()) != Some("dialect.rs")
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// No bare `?` may survive outside a string literal: every placeholder became
/// a numbered `$N`.
#[test]
fn rewritten_statements_have_no_bare_placeholders() {
    let statements = all_statements();
    assert!(
        statements.len() > 300,
        "the extractor went blind — {} statements found",
        statements.len()
    );
    for (file, sql) in &statements {
        let rewritten = sqlite_to_postgres(sql);
        let bare = masked(&rewritten).matches('?').count();
        assert_eq!(
            bare, 0,
            "{file}: bare `?` survived the rewrite: {rewritten}"
        );
    }
}

/// `$N` numbers arrive strictly in source order, 1..=N, no gaps or repeats.
#[test]
fn rewritten_placeholders_are_ordered_and_dense() {
    for (file, sql) in &all_statements() {
        let rewritten = sqlite_to_postgres(sql);
        let masked_text = masked(&rewritten);
        let mut expected = 1usize;
        let mut rest = masked_text.as_str();
        while let Some(pos) = rest.find('$') {
            let digits: String = rest[pos + 1..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            let n: usize = digits.parse().unwrap_or(0);
            assert_eq!(
                n, expected,
                "{file}: placeholder {n} out of order (expected {expected}): {rewritten}"
            );
            expected += 1;
            rest = &rest[pos + 1 + digits.len()..];
        }
    }
}

/// String literal contents pass through byte-for-byte (no rewrite touches a
/// quoted value — `'$...'` JSON paths, LIKE patterns, timestamps in text).
#[test]
fn string_literals_survive_unchanged() {
    let cases = [
        (
            "SELECT * FROM t WHERE a = 'don''t ? break'",
            "'don''t ? break'",
        ),
        ("SELECT '$.tasks' FROM j", "'$.tasks'"),
        ("SELECT 'a\\'b' FROM t", "'a\\'b'"),
    ];
    for (sql, literal) in cases {
        let rewritten = sqlite_to_postgres(sql);
        assert!(rewritten.contains(literal), "{sql} → {rewritten}");
    }
}

/// `datetime('now')` passes through untouched: the PG baseline's TEXT shims
/// accept `'now'` directly (PG parses it as the current time), so the rewriter
/// carries no datetime rule. Every timestamp stays TEXT, matching SQLite.
#[test]
fn datetime_now_passes_through_to_the_shim() {
    assert_eq!(
        sqlite_to_postgres("SELECT datetime('now')"),
        "SELECT datetime('now')"
    );
    let complex = sqlite_to_postgres(
        "UPDATE t SET x = datetime('now') WHERE y = datetime('now', '-90 seconds')",
    );
    assert_eq!(complex.matches("datetime('now')").count(), 1);
    assert!(
        complex.contains("datetime('now', '-90 seconds')"),
        "{complex}"
    );
    assert!(!complex.contains("now()"), "{complex}");
}

/// Shared statements now carry the portable `ON CONFLICT DO NOTHING`
/// themselves; the rewriter only renumbers placeholders and leaves the clause
/// untouched.
#[test]
fn on_conflict_do_nothing_passes_through() {
    let sql =
        "INSERT INTO team_members (team_id, user_id, role) VALUES (?, ?, 'owner') ON CONFLICT DO NOTHING";
    assert_eq!(
        sqlite_to_postgres(sql),
        "INSERT INTO team_members (team_id, user_id, role) VALUES ($1, $2, 'owner') \
         ON CONFLICT DO NOTHING"
    );
}

/// Portable named-target upserts (`ON CONFLICT (…) DO UPDATE … excluded.…`)
/// pass through: SQLite ≥3.24 and Postgres share the syntax, so both upsert
/// statements in the store are single shared texts, not dual bodies.
#[test]
fn named_conflict_upsert_passes_through() {
    let sql = "INSERT INTO team_resource_grants (team_id, profile, granted_by) \
               VALUES (?, ?, ?) ON CONFLICT (team_id, profile) DO UPDATE \
               SET granted_by = excluded.granted_by";
    let rewritten = sqlite_to_postgres(sql);
    assert!(rewritten.contains("VALUES ($1, $2, $3)"), "{rewritten}");
    assert!(
        rewritten.contains("ON CONFLICT (team_id, profile) DO UPDATE"),
        "{rewritten}"
    );
    assert!(rewritten.contains("excluded.granted_by"), "{rewritten}");
}

/// `INSERT OR REPLACE` is NOT rewritten — it needs an explicit conflict target
/// and must be dual-versioned instead. The rewriter must not invent one.
#[test]
fn insert_or_replace_is_left_for_dual_versions() {
    let sql = "INSERT OR REPLACE INTO job_logs (id, a) VALUES (?, ?)";
    let rewritten = sqlite_to_postgres(sql);
    assert!(rewritten.starts_with("INSERT OR REPLACE"), "{rewritten}");
}

/// Placeholders inside string literals are not numbered — the `'$.'` JSON
/// path syntax and LIKE patterns pass through.
#[test]
fn placeholders_inside_literals_are_untouched() {
    let sql = "SELECT json_extract(v, '$.a') FROM t WHERE b LIKE ? AND c = '?'";
    let rewritten = sqlite_to_postgres(sql);
    assert!(rewritten.contains("'$.a'"), "{rewritten}");
    assert!(rewritten.contains("'?'"), "{rewritten}");
    assert!(rewritten.contains("$1"), "{rewritten}");
    assert!(!rewritten.contains("$2"), "{rewritten}");
}
