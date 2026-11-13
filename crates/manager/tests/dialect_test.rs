//! Dialect rewriter invariants, checked against **every** statement in
//! `store::` — the same corpus `store_sql_validation_test` compiles.
//!
//! The rewriter is the PG port's load-bearing wall: if it mangles a statement
//! (drops a literal, misnumbers a placeholder, breaks an operator), the failure
//! surfaces on an endpoint nobody traced. These tests hold the wall:
//! - string literal contents survive byte-for-byte;
//! - no bare `?` remains outside a literal;
//! - `$N` numbering is strictly 1..=N in source order;
//! - known rewrites produce the documented PG forms.

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

/// `datetime('now')` becomes `now()` — everywhere, including inside
/// COALESCE/CASE expressions.
#[test]
fn datetime_now_becomes_now() {
    assert_eq!(sqlite_to_postgres("SELECT datetime('now')"), "SELECT now()");
    let complex = sqlite_to_postgres("UPDATE t SET x = datetime('now') WHERE y = datetime('now')");
    assert_eq!(complex.matches("now()").count(), 2);
    assert!(!complex.contains("datetime"));
}

/// `datetime('now', '-N seconds')` becomes a PG interval arithmetic.
#[test]
fn datetime_offset_becomes_interval() {
    assert_eq!(
        sqlite_to_postgres("SELECT * FROM t WHERE seen >= datetime('now', '-300 seconds')"),
        "SELECT * FROM t WHERE seen >= now() - interval '300 seconds'"
    );
    assert!(
        sqlite_to_postgres("SELECT * FROM t WHERE seen <= datetime('now', '+45 seconds')")
            .contains("now() + interval '45 seconds'")
    );
}

/// `INSERT OR IGNORE` keeps its insert and gains `ON CONFLICT DO NOTHING` at
/// the statement's end.
#[test]
fn insert_or_ignore_gains_on_conflict() {
    let sql = "INSERT OR IGNORE INTO team_members (team_id, user_id, role) VALUES (?, ?, 'owner')";
    assert_eq!(
        sqlite_to_postgres(sql),
        "INSERT INTO team_members (team_id, user_id, role) VALUES ($1, $2, 'owner') \
         ON CONFLICT DO NOTHING"
    );
}

/// A trailing `;` stays at the end, with the conflict clause before it.
#[test]
fn trailing_semicolon_keeps_conflict_clause_before_it() {
    let sql = "INSERT OR IGNORE INTO t (a) VALUES (?);";
    let rewritten = sqlite_to_postgres(sql);
    assert!(
        rewritten.ends_with("ON CONFLICT DO NOTHING;"),
        "{rewritten}"
    );
    assert!(!rewritten.contains("DO NOTHING;;"));
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
