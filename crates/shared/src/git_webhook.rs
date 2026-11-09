//! GitHub / Gitee webhook verify + push event normalization → `INPUT_GIT_*`.

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::collections::HashMap;

type HmacSha256 = Hmac<Sha256>;

pub const PROVIDER_GITHUB: &str = "github";
pub const PROVIDER_GITEE: &str = "gitee";

/// Normalized push / pull-request (or ping) event after platform parsing.
#[derive(Debug, Clone, Default)]
pub struct NormalizedGitEvent {
    /// `push` | `pull_request` | `ping`.
    pub event_type: String,
    pub repo_full_name: String,
    pub clone_url: String,
    pub git_ref: String,
    pub branch: String,
    pub tag: String,
    pub commit_sha: String,
    pub commit_message: String,
    pub sender: String,
    pub compare_url: String,
    /// Pull-request number (`""` for push events).
    pub pr_number: String,
    /// Pull-request base branch — what `branch` filters match on.
    pub pr_base_branch: String,
    /// Pull-request source branch.
    pub pr_head_branch: String,
    /// True for platform handshake / ping — do not create a job.
    pub is_ping: bool,
    /// Set when the delivery parsed but must not create a job (e.g. a
    /// pull-request `closed` action). Non-empty = record and return 200.
    pub ignored_reason: String,
}

impl NormalizedGitEvent {
    /// Build trigger inputs (`GIT_*` keys; Manager exports as `INPUT_GIT_*`).
    pub fn to_inputs(&self, provider: &str) -> HashMap<String, String> {
        let mut m = HashMap::new();
        let put = |m: &mut HashMap<String, String>, k: &str, v: &str| {
            if !v.is_empty() {
                m.insert(k.to_string(), v.to_string());
            }
        };
        put(&mut m, "GIT_PROVIDER", provider);
        put(&mut m, "GIT_EVENT", &self.event_type);
        put(&mut m, "GIT_REPO", &self.repo_full_name);
        put(&mut m, "GIT_CLONE_URL", &self.clone_url);
        put(&mut m, "GIT_REF", &self.git_ref);
        put(&mut m, "GIT_BRANCH", &self.branch);
        put(&mut m, "GIT_TAG", &self.tag);
        put(&mut m, "GIT_SHA", &self.commit_sha);
        if self.commit_sha.len() >= 7 {
            put(&mut m, "GIT_SHA_SHORT", &self.commit_sha[..7]);
        }
        put(&mut m, "GIT_COMMIT_MESSAGE", &self.commit_message);
        put(&mut m, "GIT_SENDER", &self.sender);
        put(&mut m, "GIT_COMPARE_URL", &self.compare_url);
        put(&mut m, "GIT_PR_NUMBER", &self.pr_number);
        put(&mut m, "GIT_PR_BASE", &self.pr_base_branch);
        put(&mut m, "GIT_PR_HEAD", &self.pr_head_branch);
        m
    }
}

/// Verify an HMAC-SHA256 signature header of the form `sha256=<hex>` over `body`.
/// Constant-time via `hmac::Mac::verify_slice`.
pub fn verify_hex_hmac_sha256(secret: &str, body: &[u8], signature_header: &str) -> bool {
    let Some(hex_sig) = signature_header
        .strip_prefix("sha256=")
        .or_else(|| signature_header.strip_prefix("SHA256="))
    else {
        return false;
    };
    let Ok(sig_bytes) = hex::decode(hex_sig.trim()) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&sig_bytes).is_ok()
}

/// Verify GitHub `X-Hub-Signature-256: sha256=<hex>`.
pub fn verify_github_signature(secret: &str, body: &[u8], signature_header: &str) -> bool {
    verify_hex_hmac_sha256(secret, body, signature_header)
}

/// Verify Gitee webhook password via `X-Gitee-Token` (constant-time-ish compare).
pub fn verify_gitee_token(secret: &str, token_header: Option<&str>) -> bool {
    let Some(token) = token_header.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    if token.len() != secret.len() {
        return false;
    }
    // Best-effort constant-time equality for equal-length strings.
    let mut diff = 0u8;
    for (a, b) in token.bytes().zip(secret.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

pub fn normalize_provider(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        PROVIDER_GITHUB => Some(PROVIDER_GITHUB),
        PROVIDER_GITEE => Some(PROVIDER_GITEE),
        _ => None,
    }
}

/// Parse GitHub event from JSON + `X-GitHub-Event` header value.
pub fn parse_github_event(event_name: &str, body: &Value) -> Result<NormalizedGitEvent, String> {
    let event_name = event_name.trim().to_ascii_lowercase();
    if event_name == "ping" {
        return Ok(NormalizedGitEvent {
            event_type: "ping".into(),
            repo_full_name: json_str(body, &["repository", "full_name"]),
            is_ping: true,
            ..Default::default()
        });
    }
    if event_name == "pull_request" {
        return parse_github_pull_request(body);
    }
    if event_name != "push" && !event_name.is_empty() {
        return Err(format!(
            "unsupported GitHub event '{event_name}' (only push/pull_request/ping)"
        ));
    }
    parse_push_like(body, "push", true)
}

/// GitHub `pull_request` delivery → a normalizable event.
///
/// Only `opened`, `synchronize`, `reopened`, and `ready_for_review` create
/// jobs; other actions (closed, labeled, edited, …) return an ignored event
/// so the caller records them without a redelivery-triggering error. Draft
/// PRs are ignored until `ready_for_review`. Branch filters match the **base**
/// branch (what the code merges into).
fn parse_github_pull_request(body: &Value) -> Result<NormalizedGitEvent, String> {
    let action = json_str(body, &["action"]);
    let draft = body
        .get("pull_request")
        .and_then(|pr| pr.get("draft"))
        .and_then(|draft| draft.as_bool())
        .unwrap_or(false);
    let base_branch = json_str(body, &["pull_request", "base", "ref"]);
    let head_branch = json_str(body, &["pull_request", "head", "ref"]);
    let head_sha = json_str(body, &["pull_request", "head", "sha"]);
    if head_sha.is_empty() {
        return Err("pull_request payload is missing head.sha".into());
    }
    let clone_url = {
        let ssh = json_str(body, &["repository", "ssh_url"]);
        if ssh.is_empty() {
            json_str(body, &["repository", "clone_url"])
        } else {
            ssh
        }
    };
    let mut event = NormalizedGitEvent {
        event_type: "pull_request".into(),
        repo_full_name: json_str(body, &["repository", "full_name"]),
        clone_url,
        git_ref: if base_branch.is_empty() {
            String::new()
        } else {
            format!("refs/heads/{base_branch}")
        },
        branch: base_branch,
        commit_sha: head_sha,
        commit_message: first_line_truncated(&json_str(body, &["pull_request", "title"]), 200),
        sender: json_str(body, &["sender", "login"]),
        compare_url: json_str(body, &["pull_request", "html_url"]),
        pr_number: json_number(body, &["number"]),
        pr_base_branch: json_str(body, &["pull_request", "base", "ref"]),
        pr_head_branch: head_branch,
        tag: String::new(),
        is_ping: false,
        ignored_reason: String::new(),
    };
    let actionable = matches!(
        action.as_str(),
        "opened" | "synchronize" | "reopened" | "ready_for_review"
    );
    if !actionable {
        event.ignored_reason = format!("pull_request action '{action}'");
    } else if draft && action != "ready_for_review" {
        event.ignored_reason = "draft pull request".into();
    }
    Ok(event)
}

/// Parse Gitee event from JSON + `X-Gitee-Event` header (e.g. `Push Hook`).
pub fn parse_gitee_event(event_name: &str, body: &Value) -> Result<NormalizedGitEvent, String> {
    let ev = event_name.trim().to_ascii_lowercase();
    if ev.contains("ping") || body.get("hook_name").and_then(|v| v.as_str()) == Some("ping") {
        return Ok(NormalizedGitEvent {
            event_type: "ping".into(),
            repo_full_name: gitee_repo_name(body),
            is_ping: true,
            ..Default::default()
        });
    }
    if ev.contains("merge_request") || ev.contains("merge request") {
        return parse_gitee_merge_request(body);
    }
    // Gitee uses "Push Hook" / push_hooks
    if !ev.is_empty() && !ev.contains("push") {
        return Err(format!(
            "unsupported Gitee event '{event_name}' (only Push Hook / Merge Request Hook)"
        ));
    }
    let mut ev = parse_push_like(body, "push", false)?;
    if ev.repo_full_name.is_empty() {
        ev.repo_full_name = gitee_repo_name(body);
    }
    if ev.clone_url.is_empty() {
        ev.clone_url = json_str(body, &["repository", "git_ssh_url"]);
        if ev.clone_url.is_empty() {
            ev.clone_url = json_str(body, &["repository", "ssh_url"]);
        }
    }
    if ev.sender.is_empty() {
        ev.sender = json_str(body, &["sender", "name"]);
        if ev.sender.is_empty() {
            ev.sender = json_str(body, &["user_name"]);
        }
    }
    Ok(ev)
}

/// Gitee `Merge Request Hook` → a normalized pull-request event.
fn parse_gitee_merge_request(body: &Value) -> Result<NormalizedGitEvent, String> {
    let sha = json_str(body, &["object_attributes", "last_commit", "id"]);
    if sha.is_empty() {
        return Err("merge request payload is missing last_commit.id".into());
    }
    let base = json_str(body, &["object_attributes", "target_branch"]);
    let action = json_str(body, &["object_attributes", "action"]);
    let mut event = NormalizedGitEvent {
        event_type: "pull_request".into(),
        repo_full_name: gitee_repo_name(body),
        clone_url: {
            let ssh = json_str(body, &["repository", "git_ssh_url"]);
            if ssh.is_empty() {
                json_str(body, &["repository", "ssh_url"])
            } else {
                ssh
            }
        },
        git_ref: if base.is_empty() {
            String::new()
        } else {
            format!("refs/heads/{base}")
        },
        branch: base.clone(),
        commit_sha: sha,
        commit_message: first_line_truncated(&json_str(body, &["object_attributes", "title"]), 200),
        sender: {
            let name = json_str(body, &["sender", "name"]);
            if name.is_empty() {
                json_str(body, &["user", "name"])
            } else {
                name
            }
        },
        compare_url: json_str(body, &["object_attributes", "url"]),
        pr_number: json_number(body, &["object_attributes", "iid"]),
        pr_base_branch: base,
        pr_head_branch: json_str(body, &["object_attributes", "source_branch"]),
        tag: String::new(),
        is_ping: false,
        ignored_reason: String::new(),
    };
    if !matches!(action.as_str(), "open" | "update" | "reopen" | "") {
        event.ignored_reason = format!("merge request action '{action}'");
    }
    Ok(event)
}

fn gitee_repo_name(body: &Value) -> String {
    let n = json_str(body, &["repository", "full_name"]);
    if !n.is_empty() {
        return n;
    }
    let n = json_str(body, &["repository", "path_with_namespace"]);
    if !n.is_empty() {
        return n;
    }
    let owner = json_str(body, &["repository", "owner", "login"]);
    let name = json_str(body, &["repository", "name"]);
    if !owner.is_empty() && !name.is_empty() {
        format!("{owner}/{name}")
    } else {
        name
    }
}

fn parse_push_like(
    body: &Value,
    event_type: &str,
    github: bool,
) -> Result<NormalizedGitEvent, String> {
    let git_ref = json_str(body, &["ref"]);
    if git_ref.is_empty() {
        return Err("missing ref".into());
    }
    let (branch, tag) = split_ref(&git_ref);
    let mut commit_sha = json_str(body, &["after"]);
    if commit_sha.is_empty() || commit_sha.chars().all(|c| c == '0') {
        commit_sha = json_str(body, &["head_commit", "id"]);
    }
    if commit_sha.is_empty() {
        // last commit in commits[]
        if let Some(arr) = body.get("commits").and_then(|c| c.as_array()) {
            if let Some(last) = arr.last() {
                commit_sha = json_str(last, &["id"]);
            }
        }
    }
    let mut commit_message = json_str(body, &["head_commit", "message"]);
    if commit_message.is_empty() {
        if let Some(arr) = body.get("commits").and_then(|c| c.as_array()) {
            if let Some(last) = arr.last() {
                commit_message = json_str(last, &["message"]);
            }
        }
    }
    commit_message = first_line_truncated(&commit_message, 200);

    let repo_full_name = json_str(body, &["repository", "full_name"]);
    let clone_url = if github {
        let ssh = json_str(body, &["repository", "ssh_url"]);
        if !ssh.is_empty() {
            ssh
        } else {
            json_str(body, &["repository", "clone_url"])
        }
    } else {
        String::new()
    };
    let sender = json_str(body, &["sender", "login"]);
    let compare_url = json_str(body, &["compare"]);

    Ok(NormalizedGitEvent {
        event_type: event_type.into(),
        repo_full_name,
        clone_url,
        git_ref,
        branch,
        tag,
        commit_sha,
        commit_message,
        sender,
        compare_url,
        is_ping: false,
        ..Default::default()
    })
}

fn split_ref(git_ref: &str) -> (String, String) {
    if let Some(b) = git_ref.strip_prefix("refs/heads/") {
        (b.to_string(), String::new())
    } else if let Some(t) = git_ref.strip_prefix("refs/tags/") {
        (String::new(), t.to_string())
    } else {
        (String::new(), String::new())
    }
}

fn first_line_truncated(msg: &str, max: usize) -> String {
    let line = msg.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max {
        line.to_string()
    } else {
        line.chars().take(max).collect()
    }
}

fn json_str(v: &Value, path: &[&str]) -> String {
    let mut cur = v;
    for p in path {
        cur = match cur.get(*p) {
            Some(x) => x,
            None => return String::new(),
        };
    }
    cur.as_str().unwrap_or("").trim().to_string()
}

/// Numeric payload field as a string (GitHub's `number` is an integer).
fn json_number(v: &Value, path: &[&str]) -> String {
    let mut cur = v;
    for p in path {
        cur = match cur.get(*p) {
            Some(x) => x,
            None => return String::new(),
        };
    }
    if let Some(n) = cur.as_i64() {
        return n.to_string();
    }
    cur.as_str().unwrap_or("").trim().to_string()
}

/// Commit message requests CI skip.
pub fn has_skip_ci(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("[skip ci]") || lower.contains("[ci skip]")
}

/// Resolve a stable per-delivery idempotency key.
///
/// - **GitHub**: prefer `X-GitHub-Delivery` (UUID per delivery).
/// - **Gitee**: has no per-delivery UUID; **do not** use `X-Gitee-Timestamp`.
///   Use `provider:repo:ref:sha` so retries of the same push collapse.
/// - **Ping**: returns `None` (handshake may repeat; no job).
pub fn resolve_delivery_id(
    provider: &str,
    platform_delivery_header: Option<&str>,
    event: &NormalizedGitEvent,
) -> Option<String> {
    if event.is_ping {
        return None;
    }
    if provider == PROVIDER_GITHUB {
        if let Some(h) = platform_delivery_header
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(h.to_string());
        }
    }
    // Gitee (and GitHub fallback when header missing): composite from payload.
    if event.git_ref.is_empty() && event.commit_sha.is_empty() {
        return None;
    }
    let repo = if event.repo_full_name.is_empty() {
        "_"
    } else {
        event.repo_full_name.as_str()
    };
    let sha = if event.commit_sha.is_empty() {
        "0"
    } else {
        event.commit_sha.as_str()
    };
    Some(format!("{provider}:{repo}:{}:{sha}", event.git_ref))
}

/// `branch_filter` is comma-separated patterns; `*` = glob (prefix/suffix/contains).
/// Empty filter matches all branches. Tag-only pushes match when filter empty.
pub fn branch_matches(filter: &str, branch: &str) -> bool {
    let filter = filter.trim();
    if filter.is_empty() {
        return true;
    }
    if branch.is_empty() {
        return false;
    }
    filter
        .split([',', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .any(|pat| glob_match(pat, branch))
}

fn glob_match(pat: &str, value: &str) -> bool {
    if pat == "*" {
        return true;
    }
    if let Some(prefix) = pat.strip_suffix('*') {
        if let Some(suffix) = prefix.strip_prefix('*') {
            return value.contains(suffix);
        }
        return value.starts_with(prefix);
    }
    if let Some(suffix) = pat.strip_prefix('*') {
        return value.ends_with(suffix);
    }
    pat == value
}
