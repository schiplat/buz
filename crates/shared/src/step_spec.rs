//! buz-plugin step template spec (marketplace ADR §5.1) — one file, one
//! plugin, parsed from a step index (`local` dir now, `https` later).
//!
//! Validation is shared by the `buz step validate` CLI (plugin-repo CI) and
//! the Manager's resolution path, so both reject the same files with the
//! same messages. v1 accepts only `runtime: template`; wasm is gated behind
//! Phase 3.

use serde::Deserialize;

use crate::pipeline_yaml::parse_duration_seconds;
use crate::protocol::{InputTypeKind, PipelineInputDef};

/// One `inputs:` entry of a step spec — same constraints as trigger inputs,
/// but the map key is the input name (converted into [`PipelineInputDef`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawStepInput {
    #[serde(default, rename = "type")]
    pub input_type: Option<String>,
    #[serde(default)]
    pub required: Option<bool>,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub masking: Option<bool>,
    #[serde(default)]
    pub description: Option<String>,
}

/// buz-plugin step template spec (`plugins/<id>.yaml`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepSpec {
    /// `<namespace>/<name>`, e.g. `buz/feishu` (official) or
    /// `alice/telegram` (community).
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// `X.Y.Z` (three-part).
    pub version: String,
    /// v1: only `template` (or omitted). `wasm` is Phase 3.
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub description_zh: Option<String>,
    /// Optional browsing/filtering category; not part of the template ID.
    #[serde(default)]
    pub category: Option<String>,
    /// `official` | `community` (default when omitted). Attribution rules
    /// bind to community: `author` + `source_repo` become required.
    #[serde(default)]
    pub publisher: Option<String>,
    /// Template author — required for `publisher: community` (ADR §6.7.1).
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub author_url: Option<String>,
    /// Repository the template is maintained in — required for
    /// `publisher: community`.
    #[serde(default)]
    pub source_repo: Option<String>,
    /// Search keywords (zh/en mix), consumed by the static-site index.
    #[serde(default)]
    pub keywords: Option<Vec<String>>,
    #[serde(default)]
    pub license: Option<String>,
    /// `>=X.Y.Z` or exact `X.Y.Z` — the only supported forms (2026-10-03).
    #[serde(default)]
    pub requires_buz: Option<String>,
    /// `shell` (default) | `docker` | `ssh`.
    #[serde(default)]
    pub engine: Option<String>,
    /// Required for `engine: docker`; digest-pinning recommended.
    #[serde(default)]
    pub image: Option<String>,
    /// Call-site fallback when the invoking step omits `timeout`.
    #[serde(default)]
    pub default_timeout: Option<String>,
    /// Input declarations; values arrive as `INPUT_<UPPER_SNAKE>` env.
    #[serde(default)]
    pub inputs: Option<std::collections::BTreeMap<String, RawStepInput>>,
    /// Step script. Values reach it only via environment variables —
    /// `${secrets.*}` / `${vars.*}` references are rejected.
    #[serde(default)]
    pub run: Option<String>,
}

impl StepSpec {
    /// Namespace portion of `id` (`buz/feishu` → `buz`).
    pub fn namespace(&self) -> &str {
        self.id.split('/').next().unwrap_or("")
    }

    /// Step-id portion of `id` (`buz/feishu` → `feishu`).
    pub fn step_id(&self) -> &str {
        self.id.split('/').last().unwrap_or("")
    }

    /// Effective engine (`shell` when omitted).
    pub fn effective_engine(&self) -> &str {
        self.engine.as_deref().unwrap_or("shell").trim()
    }

    /// Effective publisher — omitted means `community` (the strictest
    /// attribution default; official templates declare it explicitly).
    pub fn effective_publisher(&self) -> &'static str {
        match self.publisher.as_deref().map(str::trim) {
            Some("official") => "official",
            _ => "community",
        }
    }

    fn non_empty(field: Option<&str>) -> bool {
        field.map(str::trim).filter(|s| !s.is_empty()).is_some()
    }

    /// Attribution completeness for this spec's publisher tier.
    pub fn attribution_errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        match self.publisher.as_deref().map(str::trim) {
            None | Some("") | Some("official") | Some("community") => {}
            Some(other) => errors.push(format!(
                "publisher: unknown value {other:?} — use 'official' or 'community'"
            )),
        }
        if self.effective_publisher() == "community" {
            if !Self::non_empty(self.author.as_deref()) {
                errors.push(
                    "author: is required for publisher: community (or omitted publisher) \
                     templates — who maintains this template?"
                        .into(),
                );
            }
            if !Self::non_empty(self.source_repo.as_deref()) {
                errors.push(
                    "source_repo: is required for publisher: community (or omitted publisher) \
                     templates — where is it maintained?"
                        .into(),
                );
            }
        }
        errors
    }

    /// Input declarations with the map key filled in as `name`, preserving
    /// declaration order (BTreeMap keys).
    pub fn input_defs(&self) -> Vec<PipelineInputDef> {
        self.inputs
            .iter()
            .flatten()
            .map(|(name, raw)| PipelineInputDef {
                name: name.clone(),
                description: raw.description.clone(),
                required: raw.required,
                default: raw.default.clone(),
                masking: raw.masking,
                input_type: raw.input_type.clone(),
                pattern: raw.pattern.clone(),
                options: raw.options.clone(),
            })
            .collect()
    }
}

fn parse_version_triple(v: &str) -> Option<(u64, u64, u64)> {
    let parts: Vec<&str> = v.trim().split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].trim().parse().ok()?,
        parts[1].trim().parse().ok()?,
        parts[2].trim().parse().ok()?,
    ))
}

/// Outcome of parsing a `requires_buz` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuzRequirement {
    /// `>=X.Y.Z`
    Min((u64, u64, u64)),
    /// `X.Y.Z`
    Exact((u64, u64, u64)),
}

/// Parse `requires_buz`: only `>=X.Y.Z` and exact `X.Y.Z` are supported
/// (2026-10-03 decision); `<`, `~`, `^`, ranges are rejected with the
/// supported forms spelled out.
pub fn parse_buz_requirement(raw: &str) -> Result<BuzRequirement, String> {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix(">=") {
        let v = parse_version_triple(rest)
            .ok_or_else(|| format!("'>= {rest}' is not a X.Y.Z version"))?;
        return Ok(BuzRequirement::Min(v));
    }
    if raw.starts_with('<') || raw.starts_with('^') || raw.starts_with('~') {
        return Err(format!(
            "unsupported requires_buz '{raw}' — only '>=X.Y.Z' and exact 'X.Y.Z' are supported"
        ));
    }
    let v = parse_version_triple(raw).ok_or_else(|| {
        format!("unsupported requires_buz '{raw}' — only '>=X.Y.Z' and exact 'X.Y.Z' are supported")
    })?;
    Ok(BuzRequirement::Exact(v))
}

fn fmt_version(v: (u64, u64, u64)) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

/// Check a `requires_buz` value against the running Buz version.
///
/// The error message must say the plugin is too new (ADR §5.1) — never
/// "invalid YAML", which would send the author fixing the wrong file.
pub fn buz_requirement_satisfied(raw: &str, current_buz: &str) -> Result<(), String> {
    let current = parse_version_triple(current_buz)
        .ok_or_else(|| format!("current Buz version '{current_buz}' is not X.Y.Z"))?;
    match parse_buz_requirement(raw)? {
        BuzRequirement::Min(required) => {
            if current >= required {
                Ok(())
            } else {
                Err(format!(
                    "step requires Buz >= {} — this plugin is too new for this Manager \
                     (current {current_buz})",
                    fmt_version(required)
                ))
            }
        }
        BuzRequirement::Exact(required) => {
            if current == required {
                Ok(())
            } else {
                Err(format!(
                    "step requires exactly Buz {} — this plugin is too new (or too old) for \
                     this Manager (current {current_buz})",
                    fmt_version(required)
                ))
            }
        }
    }
}

fn valid_id_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn validate_input_default(name: &str, input: &RawStepInput, errors: &mut Vec<String>) {
    let kind = InputTypeKind::parse(input.input_type.as_deref());
    if input.input_type.as_deref().map(str::trim) == Some("host") {
        errors.push(format!(
            "inputs.{name}: type: host is not supported for marketplace step templates — use string, number, boolean, choice, or list"
        ));
    } else if kind.is_none() {
        errors.push(format!(
            "inputs.{name}: unknown type {:?} — use string, number, boolean, choice, or list",
            input.input_type.as_deref().unwrap_or("")
        ));
    }

    if kind == Some(InputTypeKind::Choice)
        && input.options.as_ref().map(|o| o.is_empty()).unwrap_or(true)
    {
        errors.push(format!(
            "inputs.{name}: type: choice requires a non-empty options list"
        ));
    }

    if input.masking.unwrap_or(false) && input.default.is_some() {
        errors.push(format!(
            "inputs.{name}: masked inputs must not declare a literal default"
        ));
    }

    let regex = match input.pattern.as_deref() {
        Some(pattern) => match regex::Regex::new(pattern) {
            Ok(regex) => Some(regex),
            Err(_) => {
                errors.push(format!("inputs.{name}: invalid pattern {pattern:?}"));
                None
            }
        },
        None => None,
    };

    let (Some(default), Some(kind)) = (input.default.as_deref(), kind) else {
        return;
    };
    let value = default.trim();
    let mut valid = true;
    let mut reason = None;
    match kind {
        InputTypeKind::String => {}
        InputTypeKind::Number => {
            if value.parse::<i64>().is_err() {
                valid = false;
                reason = Some("must be an integer");
            }
        }
        InputTypeKind::Boolean => {
            if !matches!(value.to_ascii_lowercase().as_str(), "true" | "false") {
                valid = false;
                reason = Some("must be true or false");
            }
        }
        InputTypeKind::Choice => {
            if let Some(options) = input.options.as_deref() {
                if !options.iter().any(|option| option == value) {
                    valid = false;
                    reason = Some("must match one of the declared options");
                }
            }
        }
        InputTypeKind::List => {
            let items: Vec<&str> = value
                .trim_matches('\n')
                .split('\n')
                .map(str::trim)
                .collect();
            if items.iter().any(|item| item.is_empty()) {
                valid = false;
                reason = Some("must not contain an empty list item");
            }
        }
        InputTypeKind::Host => return,
    }
    if !valid {
        errors.push(format!(
            "inputs.{name}.default: {}",
            reason.unwrap_or("is invalid")
        ));
    }

    if let Some(regex) = regex {
        let matches = if kind == InputTypeKind::List {
            value
                .trim_matches('\n')
                .split('\n')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .all(|item| regex.is_match(item))
        } else {
            regex.is_match(value)
        };
        if !matches {
            errors.push(format!(
                "inputs.{name}.default: does not match pattern {:?}",
                input.pattern.as_deref().unwrap_or("")
            ));
        }
    }
}

/// Validate a parsed step spec. Returns one message per problem so plugin
/// CI can print them all at once.
pub fn validate_step_spec(spec: &StepSpec, current_buz: &str) -> Result<(), Vec<String>> {
    let mut errors: Vec<String> = Vec::new();

    let parts: Vec<&str> = spec.id.split('/').collect();
    // Namespace is the owner identity; the second segment is the stable name.
    if parts.len() != 2 || !parts.iter().all(|seg| valid_id_segment(seg)) {
        errors.push(format!(
            "id: expected '<namespace>/<name>' with lowercase [a-z0-9-] segments, \
             got {:?}",
            spec.id
        ));
    } else {
        // Reserved namespace (ADR §6.7.1, 2026-10-04): `buz/` belongs to the
        // officially maintained set; community templates pick their own
        // namespace (usually the author handle) at first contribution.
        let official = spec.effective_publisher() == "official";
        if official && parts[0] != "buz" {
            errors.push(format!(
                "id: publisher 'official' templates must live in the 'buz' namespace as \
                 buz/<name> (e.g. buz/feishu), got {:?}",
                spec.id
            ));
        }
        if !official && parts[0] == "buz" {
            errors.push(
                "id: namespace 'buz' is reserved for officially maintained templates — \
                 pick your own namespace (usually your author handle)"
                    .into(),
            );
        }
    }

    if let Some(category) = spec.category.as_deref() {
        if category.trim().is_empty()
            || !category
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' '))
        {
            errors.push(
                "category: must be a non-empty label using letters, numbers, spaces, '-' or '_'"
                    .into(),
            );
        }
    }

    if parse_version_triple(&spec.version).is_none() {
        errors.push(format!(
            "version: must be X.Y.Z (three parts), got {:?}",
            spec.version
        ));
    }

    match spec.runtime.as_deref().map(str::trim) {
        None | Some("") | Some("template") => {}
        Some("wasm") => errors.push(
            "runtime: 'wasm' is gated behind Phase 3 and not accepted yet — use 'template'".into(),
        ),
        Some(other) => errors.push(format!(
            "runtime: unknown runtime {other:?} — use 'template'"
        )),
    }

    match spec.effective_engine() {
        // `ssh` runs on the pipeline-declared target host — reserved for the
        // community (unmanaged) tier and governed official deploy templates;
        // see the marketplace governance document in buz-plugins.
        "shell" | "ssh" => {}
        "docker" => {
            if spec
                .image
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none()
            {
                errors.push("image: is required for engine: docker".into());
            }
        }
        other => errors.push(format!(
            "engine: {other:?} is not valid for a step template — use shell, ssh, or docker"
        )),
    }

    let run = spec.run.as_deref().unwrap_or("").trim();
    if run.is_empty() {
        errors.push("run: is required (script/command the step executes)".into());
    } else {
        for banned in ["${secrets.", "${vars."] {
            if run.contains(banned) {
                errors.push(format!(
                    "run: must not reference {banned}}} — input and secret values reach the \
                     script only via INPUT_*/env variables"
                ));
            }
        }
    }

    if let Some(timeout) = spec.default_timeout.as_deref() {
        if let Err(msg) = parse_duration_seconds(timeout) {
            errors.push(format!("default_timeout: {msg}"));
        }
    }

    if let Some(raw) = spec.requires_buz.as_deref() {
        if let Err(msg) = buz_requirement_satisfied(raw, current_buz) {
            errors.push(format!("requires_buz: {msg}"));
        }
    }

    errors.extend(spec.attribution_errors());

    // Credential-like inputs must be masked: their values are redacted in
    // logs/snapshots/audit. Name-based heuristic — cheap, catches the
    // overwhelmingly common cases at authoring time.
    const CREDENTIAL_HINTS: &[&str] = &[
        "webhook",
        "token",
        "secret",
        "password",
        "passwd",
        "api_key",
        "apikey",
        "credential",
        "private_key",
        "access_key",
        "secret_key",
        "app_key",
        "sign_key",
    ];
    let mut input_env_keys: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for (name, input) in spec.inputs.iter().flatten() {
        validate_input_default(name, input, &mut errors);
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        {
            errors.push(format!(
                "inputs.{name}: name must contain only ASCII letters, numbers, '_' or '-'"
            ));
        }
        let env_key = crate::protocol::input_env_key(name);
        if let Some(previous) = input_env_keys.get(&env_key) {
            errors.push(format!(
                "inputs.{name}: normalizes to {env_key}, which is already used by input '{previous}'"
            ));
        } else {
            input_env_keys.insert(env_key, name.clone());
        }
        let lower = name.to_lowercase();
        let credential_like = lower.contains("key") && !lower.contains("keyword")
            || CREDENTIAL_HINTS.iter().any(|hint| lower.contains(hint));
        if credential_like && !input.masking.unwrap_or(false) {
            errors.push(format!(
                "inputs.{name}: credential-like input must declare `masking: true` — values \
                 would otherwise appear in logs and job snapshots"
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Parse and validate one step-spec YAML file in one call.
pub fn parse_and_validate_step_spec(raw: &str, current_buz: &str) -> Result<StepSpec, Vec<String>> {
    let spec: StepSpec = serde_yaml::from_str(raw).map_err(|e| vec![format!("YAML: {e}")])?;
    validate_step_spec(&spec, current_buz).map(|()| spec)
}
