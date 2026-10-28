//! Pure helpers for `env_render` — parse / diff / merge / round-trip of `.env`
//! files. Kept free of I/O so the compose env_file rules are unit-testable.
//!
//! The Runner's step machinery (`poll.rs`) feeds templates, values and targets
//! (local files or SSH) into these helpers; this module never touches disk.

use std::collections::HashMap;

/// A single parsed `.env` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvLine {
    Blank,
    Comment(String),
    KeyValue { key: String, value: String },
}

/// Parse `.env` content with docker-compose env_file semantics:
/// - blank lines and lines starting with `#` are skipped;
/// - `KEY=value` (value optionally single/double-quoted) is kept;
/// - other lines are ignored.
pub fn parse_env_file(content: &str) -> Vec<(String, String)> {
    parse_env_lines(content)
        .into_iter()
        .filter_map(|l| match l {
            EnvLine::KeyValue { key, value } => Some((key, value)),
            _ => None,
        })
        .collect()
}

/// Parse `.env` content preserving blank/comment/KeyValue structure.
pub fn parse_env_lines(content: &str) -> Vec<EnvLine> {
    let mut out = Vec::new();
    for line in content.lines() {
        let t = line.trim_start();
        if t.is_empty() {
            out.push(EnvLine::Blank);
            continue;
        }
        if t.starts_with('#') {
            out.push(EnvLine::Comment(t.to_string()));
            continue;
        }
        let Some((k, v)) = t.split_once('=') else {
            continue;
        };
        let key = k.trim();
        if key.is_empty() || !is_env_key(key) {
            continue;
        }
        let v = v.trim();
        let value = if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
            unquote_double(&v[1..v.len() - 1])
        } else if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
            v[1..v.len() - 1].to_string()
        } else {
            v.to_string()
        };
        out.push(EnvLine::KeyValue {
            key: key.to_string(),
            value,
        });
    }
    out
}

fn unquote_double(inner: &str) -> String {
    // docker compose double-quote escapes: `\"` → `"`, `\\` → `\`.
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                if next == '"' || next == '\\' {
                    out.push(next);
                    chars.next();
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

/// Quote a value for a compose env_file line. Values containing whitespace,
/// `#`, `=`, `"` or empty values are written double-quoted (matching
/// `unquote_double`).
pub fn quote_env_value(value: &str) -> String {
    let needs_quote = value.is_empty()
        || value.starts_with(char::is_whitespace)
        || value.ends_with(char::is_whitespace)
        || value.contains('#')
        || value.contains('=')
        || value.contains('"')
        || value.contains('\\');
    if needs_quote {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        value.to_string()
    }
}

fn is_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Diff old vs new `.env` content; returns keys whose value changed, was
/// added, or was removed (only keys present in `new` are reported — the
/// template is the source of truth for the keys it defines). Keys are
/// reported in the order they appear in `new`.
pub fn diff_env(old: &str, new: &str) -> Vec<String> {
    let old_map: HashMap<String, String> = parse_env_file(old).into_iter().collect();
    let mut changed = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (k, v) in parse_env_file(new) {
        if !seen.insert(k.clone()) {
            continue;
        }
        match old_map.get(&k) {
            Some(old_v) if old_v == &v => {}
            _ => changed.push(k),
        }
    }
    changed
}

/// Merge a freshly rendered template over the existing `.env`:
/// - template lines are the source of truth for the keys they define;
/// - comment / blank lines from the rendered template are preserved verbatim
///   (so `# WARNING: ...` lines written by `on_missing: warn` survive);
/// - existing keys the template does **not** define (operator-added extras)
///   are appended, preserving their values;
/// - returns (merged content, changed keys).
///
/// Multiline values can never reach here: `value_source::expand` rejects them
/// (Strict mode) before rendering, and parsed `.env` lines cannot contain
/// newlines by construction.
pub fn merge_env(old: &str, rendered: &str) -> (String, Vec<String>) {
    let old_entries = parse_env_file(old);
    let rendered_lines = parse_env_lines(rendered);

    let rendered_keys: std::collections::HashSet<&str> = rendered_lines
        .iter()
        .filter_map(|l| match l {
            EnvLine::KeyValue { key, .. } => Some(key.as_str()),
            _ => None,
        })
        .collect();

    let mut content = String::with_capacity(old.len() + rendered.len() + 64);
    for line in &rendered_lines {
        match line {
            EnvLine::Blank => content.push('\n'),
            EnvLine::Comment(c) => {
                content.push_str(c);
                content.push('\n');
            }
            EnvLine::KeyValue { key, value } => {
                content.push_str(key);
                content.push('=');
                content.push_str(&quote_env_value(value));
                content.push('\n');
            }
        }
    }
    for (key, value) in &old_entries {
        if rendered_keys.contains(key.as_str()) {
            continue;
        }
        content.push_str(key);
        content.push('=');
        content.push_str(&quote_env_value(value));
        content.push('\n');
    }

    let changed = diff_env(old, &content);
    (content, changed)
}

/// Round-trip guard (Review R2 #5): after rendering, re-parse the written
/// content with the same compose env_file rules and assert every key
/// round-trips to its rendered value. Catches parser quirks before the file
/// lands on disk.
pub fn round_trip_check(content: &str) -> Result<(), String> {
    let entries = parse_env_file(content);
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (key, value) in &entries {
        if !seen.insert(key) {
            return Err(format!("duplicate key '{key}' in rendered output"));
        }
        let line = format!("{key}={}\n", quote_env_value(value));
        let reparsed = parse_env_file(&line);
        if reparsed.first() != Some(&(key.clone(), value.clone())) {
            return Err(format!(
                "key '{key}' did not round-trip through compose env_file quoting"
            ));
        }
    }
    Ok(())
}
