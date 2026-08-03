//! Parse secret / var purpose-slot references used in pipeline YAML.
//!
//! Preferred form: `${secrets.NAME}` (aligned with config.render).
//! Bare `NAME` remains accepted for migration on purpose slots
//! (`registry_secret`, `config.source`).

/// Extract `NAME` from `${secrets.NAME}` (trimmed). Returns `None` if the
/// string is not exactly that form (optional whitespace around the whole expr).
pub fn parse_secrets_expr(raw: &str) -> Option<&str> {
    parse_domain_expr(raw, "secrets")
}

/// Extract `NAME` from `${vars.NAME}`.
pub fn parse_vars_expr(raw: &str) -> Option<&str> {
    parse_domain_expr(raw, "vars")
}

fn parse_domain_expr<'a>(raw: &'a str, domain: &str) -> Option<&'a str> {
    let s = raw.trim();
    let prefix = format!("${{{domain}.");
    let rest = s.strip_prefix(&prefix)?.strip_suffix('}')?;
    if rest.is_empty() {
        return None;
    }
    if !rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(rest)
}

/// Resolve a purpose-slot value to a secret name.
///
/// Accepts `${secrets.NAME}` or a bare `NAME` (UPPER_SNAKE-style identifier).
pub fn resolve_secret_slot(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(name) = parse_secrets_expr(s) {
        return Some(name.to_string());
    }
    // Bare name — reject if it looks like an unresolved expression.
    if s.contains('$') || s.contains('{') || s.contains('}') {
        return None;
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(s.to_string())
}

/// True when `run` contains a `${secrets.` reference (not expanded in scripts).
pub fn run_contains_secrets_expr(run: &str) -> bool {
    run.contains("${secrets.")
}

/// Collect distinct `${secrets.NAME}` identifiers from arbitrary text
/// (env values, templates, etc.). Order is first-seen.
pub fn collect_secrets_names(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    const MARKER: &str = "${secrets.";
    while let Some(i) = rest.find(MARKER) {
        rest = &rest[i + MARKER.len()..];
        let Some(end) = rest.find('}') else { break };
        let name = &rest[..end];
        if !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !out.iter().any(|x| x == name)
        {
            out.push(name.to_string());
        }
        rest = &rest[end + 1..];
    }
    out
}
