//! Unified value resolution for `${secrets.X}` / `${vars.X}` references.
//!
//! Sits next to `log_masking` with the same producer/consumer shape — one
//! expander, one `ValueMap`, callers choose strictness. Consumers:
//!
//! - **Manager**: `config_path` templating (`Lenient` mode, legacy `$tag`
//!   syntax, unresolved → empty string).
//! - **Runner**: `env_render` `.env` generation (`Strict` mode, fail-fast on
//!   unresolved references, residual detection against compose interpolation).

use std::collections::HashMap;

/// Where a reference resolves its value from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueDomain {
    /// `${secrets.X}` — sensitive; feeds the log-masking catalog.
    Secrets,
    /// `${vars.X}` — non-sensitive; render/expand only.
    Vars,
}

/// Strictness chosen by the caller, not baked into the expander.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpandMode {
    /// Legacy lenient behavior: unresolved refs expand to empty, non-reference
    /// `$` kept as-is, no residual check. Used for `config_path` / host names.
    Lenient,
    /// Fail-fast: unresolved references and leftover `${...}` are errors.
    /// Used for `env_render`.
    Strict,
}

/// What to do with a reference that cannot be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingPolicy {
    /// Abort with `RenderError::Missing`.
    Fail,
    /// Write `KEY=` empty with a `# WARNING: ... missing` comment above it.
    /// Intended for rollback deploys that reference secrets deleted since.
    Warn,
    /// Expand to the empty string (legacy `config_path` behavior).
    Blank,
}

/// The value lookups available to one expansion.
#[derive(Debug, Clone, Default)]
pub struct ValueMap {
    /// Secret `name` -> value (sensitive). Secrets are injected as env vars
    /// under the same key and matched by `${secrets.X}`.
    pub secrets: HashMap<String, String>,
    /// env key -> value (pipeline env + trigger inputs; `masking` inputs must
    /// be excluded by the builder).
    pub vars: HashMap<String, String>,
}

impl ValueMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, domain: ValueDomain, name: &str) -> Option<&str> {
        let m = match domain {
            ValueDomain::Secrets => &self.secrets,
            ValueDomain::Vars => &self.vars,
        };
        m.get(name).map(|s| s.as_str())
    }
}

/// Result of a successful expansion.
#[derive(Debug, Clone)]
pub struct Expanded {
    /// The rendered template content (may contain `KEY=` + `# WARNING`
    /// comment lines when `MissingPolicy::Warn` resolved references).
    pub content: String,
    /// References that could not be resolved (`${secrets.X}` form), in the
    /// order encountered. Empty unless `MissingPolicy::Warn` was used.
    pub missing: Vec<String>,
}

/// Expansion failures.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RenderError {
    #[error(
        "unresolved reference {reference} (secret may be missing, renamed, or on another team)"
    )]
    Missing { reference: String },
    #[error("invalid env key '{key}' on line {line} (must match [A-Za-z_][A-Za-z0-9_]*)")]
    InvalidKey { key: String, line: usize },
    #[error("duplicate key '{key}' in template")]
    DuplicateKey { key: String },
    #[error(
        "invalid reference {reference} — use ${{secrets.NAME}} or ${{vars.NAME}} in env templates"
    )]
    InvalidReference { reference: String },
    #[error("leftover template reference after rendering: {detail} (compose would re-interpolate it against host env)")]
    Residual { detail: String },
    #[error("value containing '${{' rejected for env file: {detail} (compose interpolates '${{...}}' inside env_file values)")]
    ComposeInterpolate { detail: String },
    #[error("resolved value for {reference} is multiline and cannot be written to a compose .env file (reference a file path instead)")]
    MultilineValue { reference: String },
}

/// Render `template` (an `.env`-shaped file) against `map`.
///
/// Line semantics:
/// - blank lines pass through;
/// - lines starting with `#` pass through verbatim;
/// - `KEY=value` lines are expanded (KEY validated when `mode == Strict`);
/// - anything else passes through verbatim.
///
/// The template's trailing-newline shape is preserved: a template ending with
/// `\n` yields output ending with `\n` (so single-line `config_path` templates
/// stay single-line).
pub fn expand(
    template: &str,
    map: &ValueMap,
    mode: ExpandMode,
    missing: MissingPolicy,
) -> Result<Expanded, RenderError> {
    let mut content = String::with_capacity(template.len() + 64);
    let mut all_missing: Vec<String> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();

    for (i, raw_line) in template.lines().enumerate() {
        let line = raw_line;
        if line.is_empty() {
            content.push('\n');
            continue;
        }
        if line.starts_with('#') {
            content.push_str(line);
            content.push('\n');
            continue;
        }
        let Some((key_raw, value_raw)) = line.split_once('=') else {
            if mode == ExpandMode::Lenient {
                // Lenient consumers (config_path) pass bare strings without `=`;
                // expand the whole line as a value.
                let (expanded, _line_missing) = expand_value(line, map, mode)?;
                content.push_str(&expanded);
            } else {
                content.push_str(line);
            }
            content.push('\n');
            continue;
        };
        let key = key_raw.trim();
        if !is_valid_env_key(key) {
            if mode == ExpandMode::Strict {
                return Err(RenderError::InvalidKey {
                    key: key.to_string(),
                    line: i + 1,
                });
            }
            content.push_str(line);
            content.push('\n');
            continue;
        }
        if let Some(_prev) = seen.insert(key.to_string(), i + 1) {
            if mode == ExpandMode::Strict {
                return Err(RenderError::DuplicateKey {
                    key: key.to_string(),
                });
            }
        }

        let (expanded_value, line_missing) = expand_value(value_raw, map, mode)?;

        match missing {
            MissingPolicy::Fail => {
                if let Some(reference) = line_missing.first() {
                    return Err(RenderError::Missing {
                        reference: reference.clone(),
                    });
                }
                push_key_value(&mut content, key, &expanded_value);
            }
            MissingPolicy::Warn if !line_missing.is_empty() => {
                for reference in &line_missing {
                    content.push_str(&format!(
                        "# WARNING: {} missing at {}\n",
                        reference,
                        chrono::Utc::now().to_rfc3339()
                    ));
                }
                content.push_str(key);
                content.push_str("=\n");
                all_missing.extend(line_missing);
            }
            MissingPolicy::Blank | MissingPolicy::Warn => {
                push_key_value(&mut content, key, &expanded_value);
                all_missing.extend(line_missing);
            }
        }
    }

    // Preserve the template's trailing-newline shape: a template without a
    // trailing `\n` (e.g. a single-line `config_path`) yields output without
    // one too.
    if !template.ends_with('\n') && content.ends_with('\n') {
        content.pop();
    }

    // Residual / compose guard (Strict only): after rendering, no `${` may
    // remain in non-comment lines — compose re-interpolates `${...}` inside an
    // env_file against host env, silently. WARNING comments deliberately name
    // the unresolved reference and are exempt.
    if mode == ExpandMode::Strict {
        for raw_line in content.split('\n') {
            let line = raw_line.trim_start_matches(' ');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(idx) = line.find("${") {
                let start = idx.saturating_sub(40);
                let detail: String = line
                    .chars()
                    .skip(start)
                    .take(120)
                    .collect::<String>()
                    .replace('\n', "\\n");
                return Err(RenderError::ComposeInterpolate { detail });
            }
        }
    }

    Ok(Expanded {
        content,
        missing: all_missing,
    })
}

fn push_key_value(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push('=');
    out.push_str(value);
    out.push('\n');
}

fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Expand `$`-references inside one value.
fn expand_value(
    value: &str,
    map: &ValueMap,
    mode: ExpandMode,
) -> Result<(String, Vec<String>), RenderError> {
    let mut out = String::with_capacity(value.len());
    let mut missing: Vec<String> = Vec::new();
    let mut rest = value;

    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(braced) = after.strip_prefix('{') {
            match braced.find('}') {
                Some(close) => {
                    let inner = &braced[..close];
                    match resolve_reference(inner, map, mode) {
                        Resolved::Value(v) => {
                            // A newline inside a resolved value would corrupt
                            // every following `.env` line — fail fast (Strict
                            // only; lenient config_path keeps legacy behavior).
                            if mode == ExpandMode::Strict && v.contains(['\n', '\r']) {
                                return Err(RenderError::MultilineValue {
                                    reference: format!("${{{inner}}}"),
                                });
                            }
                            out.push_str(&v);
                        }
                        Resolved::Missing(reference) => missing.push(reference),
                    }
                    rest = &braced[close + 1..];
                }
                None => {
                    out.push('$');
                    rest = after;
                }
            }
        } else if let Some(rest_after) = after.strip_prefix('$') {
            // Literal `$$` (compose escapes `$` as `$$`) — keep verbatim.
            out.push_str("$$");
            rest = rest_after;
        } else {
            let name_len = after
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                .count();
            if name_len > 0 {
                let name = &after[..name_len];
                if mode == ExpandMode::Strict {
                    return Err(RenderError::InvalidReference {
                        reference: format!("${name}"),
                    });
                }
                if let Some(v) = lookup_legacy(name, map) {
                    out.push_str(v);
                } // legacy lenient: unresolved → empty
                rest = &after[name_len..];
            } else {
                out.push('$');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok((out, missing))
}

enum Resolved {
    Value(String),
    Missing(String),
}

fn resolve_reference(inner: &str, map: &ValueMap, mode: ExpandMode) -> Resolved {
    let reference = format!("${{{inner}}}");

    // Hard-required marker: ${secrets.X:?}
    let (inner, hard_required) = match inner.strip_suffix(":?") {
        Some(rest) => (rest, true),
        None => (inner, false),
    };

    // Default fallback: ${secrets.X:-default} / ${vars.X:-default}
    let (inner, default) = match inner.split_once(":-") {
        Some((main, def)) => (main, Some(def.to_string())),
        None => (inner, None),
    };

    // Domain prefix.
    let (domain, name) = if let Some(rest) = inner.strip_prefix("secrets.") {
        (Some(ValueDomain::Secrets), rest)
    } else if let Some(rest) = inner.strip_prefix("vars.") {
        (Some(ValueDomain::Vars), rest)
    } else {
        (None, inner)
    };

    let lookup = match domain {
        Some(d) => map.get(d, name),
        None => {
            if mode == ExpandMode::Strict {
                return Resolved::Missing(reference);
            }
            // Legacy: vars then secrets (matches the old merged lookup map).
            lookup_legacy(name, map)
        }
    };

    match lookup {
        Some(v) if !v.is_empty() => Resolved::Value(v.to_string()),
        Some(_) if hard_required => Resolved::Missing(reference),
        Some(_) => match default {
            Some(d) => Resolved::Value(d),
            None => Resolved::Missing(reference),
        },
        None if hard_required => Resolved::Missing(reference),
        None => match default {
            Some(d) => Resolved::Value(d),
            None => Resolved::Missing(reference),
        },
    }
}

fn lookup_legacy<'a>(name: &str, map: &'a ValueMap) -> Option<&'a str> {
    map.vars
        .get(name)
        .map(|s| s.as_str())
        .or_else(|| map.secrets.get(name).map(|s| s.as_str()))
}

#[cfg(test)]
#[path = "value_source_tests.rs"]
mod tests;
