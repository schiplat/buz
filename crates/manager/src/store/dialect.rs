//! Dialect translation: SQLite statement text → Postgres statement text.
//!
//! The store's statements are written in SQLite dialect and shared by both
//! backends. At execution time on Postgres, [`sqlite_to_postgres`] rewrites the
//! mechanical differences so ~95% of statements need no second copy:
//!
//! - `?` placeholders → numbered `$1, $2, …` (string literals are skipped);
//! - `datetime('now')` → `datetime()` (the baseline's TEXT shim — every
//!   timestamp column is TEXT, so a `now()` timestamptz would break text
//!   comparisons);
//! - `datetime('now', '-N seconds')` → `datetime(datetime(), '-N seconds')`
//!   (same shim, same TEXT output).
//!
//! Statements whose SQLite form has no faithful mechanical translation
//! (`INSERT OR REPLACE`, `json_each`/`json_extract` walks, `strftime` epochs,
//! `datetime(?)` casts) must carry an explicit Postgres variant beside the
//! SQLite one (`Sql::Dual`) instead of leaning on this function.
//!
//! Invariants tested in `dialect_test`: string literal contents pass through
//! untouched, no bare `?` survives outside a literal, and `$N` numbers arrive
//! strictly in source order starting at 1.

/// Rewrite one SQLite statement into its Postgres form.
pub fn sqlite_to_postgres(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len() + 24);
    let mut placeholder = 0usize;
    let mut i = 0usize;
    let mut in_literal = false;

    while i < chars.len() {
        let c = chars[i];

        if in_literal {
            out.push(c);
            if c == '\'' {
                // '' is the SQL escape for one quote — stay in the literal.
                if chars.get(i + 1) == Some(&'\'') {
                    out.push('\'');
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
            out.push(c);
            i += 1;
            continue;
        }

        if starts_with(&chars, i, "datetime('now')") {
            // The baseline shim returns SQLite's TEXT format; `now()` would be
            // a timestamptz and break every text comparison against a column.
            out.push_str("datetime()");
            i += "datetime('now')".len();
            continue;
        }

        // `datetime('now', '-N seconds')` → `datetime(datetime(), '-N seconds')`
        // (only minus appears in the codebase; plus is handled for symmetry).
        if starts_with(&chars, i, "datetime('now', '-")
            || starts_with(&chars, i, "datetime('now', '+")
        {
            let minus = chars[i + 17] == '-';
            let digits_start = i + "datetime('now', '".len() + 1;
            let mut j = digits_start;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            let tail = " seconds')";
            if j > digits_start && starts_with(&chars, j, tail) {
                let seconds: String = chars[digits_start..j].iter().collect();
                let op = if minus { '-' } else { '+' };
                out.push_str(&format!("datetime(datetime(), '{op}{seconds} seconds')"));
                i = j + tail.len();
                continue;
            }
        }

        if c == '?' {
            placeholder += 1;
            out.push_str(&format!("${placeholder}"));
            i += 1;
            continue;
        }

        out.push(c);
        i += 1;
    }

    out
}

fn starts_with(chars: &[char], at: usize, needle: &str) -> bool {
    let n: Vec<char> = needle.chars().collect();
    at + n.len() <= chars.len() && chars[at..at + n.len()] == n[..]
}
