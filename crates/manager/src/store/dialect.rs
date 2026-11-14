//! Dialect translation: shared statement text → Postgres statement text.
//!
//! The store's shared statements are written in a dialect-neutral subset and
//! used by both backends. At execution time on Postgres, [`sqlite_to_postgres`]
//! performs exactly one mechanical translation:
//!
//! - `?` placeholders → numbered `$1, $2, …` (string literals are skipped).
//!
//! Everything else must already be portable. `datetime('now')` (and the
//! `'now'`-with-modifier form) passes through to the PG baseline's TEXT shims,
//! which produce SQLite's exact text format; portable upserts are written as
//! `ON CONFLICT DO NOTHING` / `ON CONFLICT (…) DO UPDATE` directly. Statements
//! whose SQLite form has no portable spelling (`json_each`/`json_extract`
//! walks, `strftime` epochs) carry an explicit Postgres variant beside the
//! SQLite one (`Sql::Dual`) instead of leaning on this function.
//!
//! Invariants tested in `dialect_test`: string literal contents pass through
//! untouched, no bare `?` survives outside a literal, and `$N` numbers arrive
//! strictly in source order starting at 1.

/// Rewrite one shared statement into its Postgres form.
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

#[cfg(test)]
mod tests {
    use super::sqlite_to_postgres;

    #[test]
    fn placeholders_are_numbered_in_source_order() {
        assert_eq!(
            sqlite_to_postgres("SELECT * FROM t WHERE a = ? AND b = ?"),
            "SELECT * FROM t WHERE a = $1 AND b = $2"
        );
    }
}
