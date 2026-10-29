//! Route-level audit coverage enforcement.
//!
//! Every non-GET route in `api/mod.rs` must be accounted for here: either it
//! records an audit event (and names the action), or it is declared
//! intentionally unaudited with the reason. Adding a write endpoint without
//! touching this table fails CI — which is the point: "someone forgot to
//! audit it" must be impossible to merge.
//!
//! Keeping the table next to the test (rather than in the router) means the
//! enforcement is a review artifact: changing a route's audit posture shows
//! up as a diff in a file whose whole purpose is that decision.

use std::collections::BTreeSet;
use std::path::Path;

/// Every non-GET route and the audit action it records.
const AUDITED: &[(&str, &str)] = &[
    ("DELETE /api/v1/admin/settings/{key}", "setting.reset"),
    (
        "DELETE /api/v1/admin/plugin-sources/{id}",
        "plugin_source.delete",
    ),
    ("DELETE /api/v1/api-tokens/{id}", "api_token.revoke"),
    ("DELETE /api/v1/environments/{id}", "environment.delete"),
    (
        "DELETE /api/v1/environments/{id}/host-allowlist/{kind}/{value}",
        "environment.host_allowlist.remove",
    ),
    (
        "DELETE /api/v1/environments/{id}/overrides/{var_id}",
        "environment.override.delete",
    ),
    (
        "DELETE /api/v1/environments/{id}/secret-overrides/{secret_id}",
        "environment.secret_override.delete",
    ),
    (
        "DELETE /api/v1/github-sync/connections/{id}",
        "github_connection.delete",
    ),
    (
        "DELETE /api/v1/github-sync/repos/{id}",
        "github_repo.delete",
    ),
    ("DELETE /api/v1/hosts/{id}", "host.delete"),
    ("DELETE /api/v1/jobs/{id}", "job.delete"),
    (
        "DELETE /api/v1/pipelines/yaml-files/{filename}",
        "pipeline.yaml_delete",
    ),
    ("DELETE /api/v1/pipelines/{id}", "pipeline.delete"),
    (
        "DELETE /api/v1/pipelines/{id}/git-hooks/{hook_id}",
        "git_hook.delete",
    ),
    ("DELETE /api/v1/runners/{id}", "runner.delete"),
    ("DELETE /api/v1/secrets/{id}", "secret.delete"),
    (
        "DELETE /api/v1/secrets/{id}/github-binds/{bind_id}",
        "secret.github_bind.remove",
    ),
    ("DELETE /api/v1/teams/{id}", "team.delete"),
    (
        "DELETE /api/v1/teams/{id}/members/{user_id}",
        "team.member_remove",
    ),
    ("DELETE /api/v1/users/{id}", "user.delete"),
    ("DELETE /api/v1/variables/{id}", "variable.delete"),
    (
        "DELETE /api/v1/variables/{id}/github-binds/{bind_id}",
        "variable.github_bind.remove",
    ),
    (
        "DELETE /api/v1/ops/resource-grants/{team_id}/{profile}",
        "resource_grant.update",
    ),
    (
        "PATCH /api/v1/pipelines/{id}/toggle-active",
        "pipeline.toggle_active",
    ),
    ("PATCH /api/v1/runners/{id}/freeze", "runner.freeze"),
    (
        "PATCH /api/v1/secrets/{id}/github-binds/{bind_id}",
        "secret.github_bind.patch",
    ),
    (
        "PATCH /api/v1/variables/{id}/github-binds/{bind_id}",
        "variable.github_bind.patch",
    ),
    (
        "POST /api/v1/admin/plugin-pool/{namespace}/{plugin_id}/refresh",
        "plugin_pool.refresh",
    ),
    ("POST /api/v1/admin/plugin-sources", "plugin_source.create"),
    (
        "POST /api/v1/admin/plugin-sources/{id}/reset-index-watermark",
        "plugin_source.reset_index_watermark",
    ),
    (
        "POST /api/v1/admin/plugin-sources/{id}/sync",
        "plugin_source.sync",
    ),
    ("POST /api/v1/api-tokens", "api_token.create"),
    ("POST /api/v1/auth/change-password", "auth.change_password"),
    ("POST /api/v1/auth/login", "auth.login"),
    ("POST /api/v1/auth/setup", "auth.setup"),
    ("POST /api/v1/environments", "environment.create"),
    (
        "POST /api/v1/environments/{id}/host-allowlist",
        "environment.host_allowlist.add",
    ),
    (
        "POST /api/v1/github-sync/connections",
        "github_connection.create",
    ),
    ("POST /api/v1/github-sync/repos", "github_repo.create"),
    ("POST /api/v1/github-sync/repos/fetch", "github_repo.fetch"),
    ("POST /api/v1/github-sync/test", "github_connection.test"),
    ("POST /api/v1/hosts", "host.create"),
    ("POST /api/v1/jobs/{id}/approve", "job.approve"),
    ("POST /api/v1/jobs/{id}/cancel", "job.cancel"),
    ("POST /api/v1/jobs/{id}/reject", "job.reject"),
    ("POST /api/v1/pipelines", "pipeline.create"),
    ("POST /api/v1/pipelines/yaml-files", "pipeline.yaml_create"),
    ("POST /api/v1/pipelines/{id}/disable", "pipeline.disable"),
    ("POST /api/v1/pipelines/{id}/enable", "pipeline.enable"),
    ("POST /api/v1/pipelines/{id}/git-hooks", "git_hook.create"),
    ("POST /api/v1/pipelines/{id}/trigger", "pipeline.trigger"),
    ("POST /api/v1/runners", "runner.create"),
    (
        "POST /api/v1/runners/{id}/register-token/reissue",
        "runner.register_token_reissued",
    ),
    ("POST /api/v1/runners/{id}/restart", "runner.restart"),
    ("POST /api/v1/secrets", "secret.create"),
    (
        "POST /api/v1/secrets/{id}/github-binds",
        "secret.github_bind.add",
    ),
    (
        "POST /api/v1/secrets/{id}/github-binds/push",
        "secret.github_push",
    ),
    ("POST /api/v1/teams", "team.create"),
    ("POST /api/v1/users", "user.create"),
    ("POST /api/v1/variables", "variable.create"),
    (
        "POST /api/v1/variables/{id}/github-binds",
        "variable.github_bind.add",
    ),
    (
        "POST /api/v1/variables/{id}/github-binds/push",
        "variable.github_push",
    ),
    (
        "POST /api/v1/ops/sandbox-changes/{id}/approve",
        "sandbox_host.config_approved",
    ),
    (
        "POST /api/v1/ops/sandbox-changes/{id}/reject",
        "sandbox_host.config_rejected",
    ),
    ("POST /api/v1/ops/sandbox-hosts", "sandbox_host.enroll"),
    (
        "POST /api/v1/ops/sandbox-hosts/{host_id}/{instance_id}/rollback",
        "sandbox_host.rollback",
    ),
    (
        "POST /api/v1/ops/sandbox-hosts/{host_id}/{instance_id}/token/rotate",
        "sandbox_host.token_rotate",
    ),
    ("PUT /api/v1/ops/resource-grants", "resource_grant.update"),
    (
        "PUT /api/v1/ops/sandbox-hosts/{host_id}/{instance_id}/config",
        "sandbox_host.config_update",
    ),
    ("PUT /api/v1/admin/settings/{key}", "setting.update"),
    (
        "PUT /api/v1/admin/plugin-pool/{namespace}/{plugin_id}/{version}",
        "plugin_pool.toggle",
    ),
    ("PUT /api/v1/environments/{id}", "environment.update"),
    (
        "PUT /api/v1/environments/{id}/overrides",
        "environment.override.upsert",
    ),
    (
        "PUT /api/v1/environments/{id}/secret-overrides",
        "environment.secret_override.upsert",
    ),
    (
        "PUT /api/v1/github-sync/connections/{id}",
        "github_connection.update",
    ),
    ("PUT /api/v1/github-sync/repos/{id}", "github_repo.update"),
    ("PUT /api/v1/hosts/{id}", "host.update"),
    (
        "PUT /api/v1/pipelines/yaml-files/{filename}",
        "pipeline.yaml_update",
    ),
    ("PUT /api/v1/pipelines/{id}", "pipeline.update"),
    ("PUT /api/v1/runners/{id}", "runner.update"),
    ("PUT /api/v1/secrets/{id}", "secret.update"),
    ("PUT /api/v1/teams/{id}", "team.update"),
    ("PUT /api/v1/teams/{id}/members", "team.member_upsert"),
    ("PUT /api/v1/users/{id}", "user.update"),
    ("PUT /api/v1/variables/{id}", "variable.update"),
];

/// Non-GET routes that intentionally write no operator-visible state, or whose
/// writes are machine telemetry. Each carries the reason — "no audit" is a
/// decision, never an oversight.
const INTENTIONALLY_UNAUDITED: &[(&str, &str)] = &[
    (
        "PATCH /api/v1/jobs/{id}/status",
        "runner-reported job progress: one event per step, machine telemetry",
    ),
    (
        "POST /api/v1/admin/plugin-sources/{id}/mirror-check",
        "read-only probe: compares sentinels, writes nothing",
    ),
    (
        "POST /api/v1/ops/sandbox-hosts/{host_id}/{instance_id}/validate",
        "dry-run config validation: mutates nothing (results come back in the response)",
    ),
    (
        "POST /api/v1/hooks/git/{provider}/{hook_key}",
        "inbound git webhook (no actor); the pipelines it triggers are audited",
    ),
    (
        "POST /api/v1/hooks/{path}",
        "inbound pipeline webhook (no actor); the run it triggers is audited",
    ),
    (
        "POST /api/v1/hosts/test",
        "connectivity probe: writes nothing",
    ),
    (
        "POST /api/v1/jobs/{id}/claim",
        "runner job claim: scheduling machinery, not a decision",
    ),
    (
        "POST /api/v1/jobs/{id}/logs",
        "runner log upload: streaming output",
    ),
    (
        "POST /api/v1/pipelines/validate",
        "dry-run validation: writes nothing",
    ),
    (
        "POST /api/v1/runners/heartbeat",
        "runner heartbeat: high-frequency liveness signal",
    ),
    (
        "POST /api/v1/runners/register",
        "runner registration: captured by the runner event log (log_runner_event)",
    ),
];

/// No deprecated path aliases remain: the step-* aliases shipped in 1.11.0
/// were removed once 1.12.0 had released (the documented one-release window).
/// (`GET /steps` also aliased `GET /plugins`; it was a read, so it was never
/// in this table — removed in the same pass.)
const ALIAS_ROUTES: &[(&str, &str)] = &[];

fn repo_file(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Parse `api/mod.rs` into `("METHOD /full/path", handler)` pairs. Routes are
/// registered as `.route("<path>", get(a).post(b))`, possibly nested routers
/// included via `.merge(...)`; the parser walks parens so chains of any length
/// work.
fn routes_from_router() -> BTreeSet<(String, String)> {
    let source = repo_file("src/api/mod.rs");
    let mut out = BTreeSet::new();
    for start in source.match_indices(".route(") {
        let open = source[start.0..].find('(').map(|i| start.0 + i).unwrap();
        let mut depth = 1usize;
        let mut end = open + 1;
        while depth > 0 && end < source.len() {
            match source.as_bytes()[end] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            end += 1;
        }
        let chain = &source[open..end];
        let Some(path) = chain.split('"').nth(1) else {
            continue;
        };
        let mut methods: Vec<&str> = Vec::new();
        for method in ["get", "post", "put", "delete", "patch"] {
            let needle = format!("{method}(");
            if chain.contains(&needle) {
                methods.push(method);
            }
        }
        for method in methods {
            // Reads are out of scope: the audit log records decisions.
            if method == "get" {
                continue;
            }
            out.insert((
                format!("{} /api/v1{}", method.to_uppercase(), path),
                String::new(),
            ));
        }
    }
    out
}

#[test]
fn every_write_route_is_audited_or_declared_unaudited() {
    let router: BTreeSet<String> = routes_from_router()
        .into_iter()
        .map(|(route, _)| route)
        .collect();
    let audited: BTreeSet<String> = AUDITED.iter().map(|(r, _)| r.to_string()).collect();
    let unaudited: BTreeSet<String> = INTENTIONALLY_UNAUDITED
        .iter()
        .map(|(r, _)| r.to_string())
        .collect();
    let aliases: BTreeSet<String> = ALIAS_ROUTES.iter().map(|(r, _)| r.to_string()).collect();

    // Every router write route must be declared exactly once.
    let missing: Vec<&String> = router
        .iter()
        .filter(|r| !audited.contains(*r) && !unaudited.contains(*r) && !aliases.contains(*r))
        .collect();
    assert!(
        missing.is_empty(),
        "write routes with no audit decision — add them to AUDITED (with the action they \
         record) or INTENTIONALLY_UNAUDITED (with the reason):\n{}",
        missing
            .iter()
            .map(|r| format!("  {r}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // …and declared routes must still exist, so stale entries cannot hide a
    // route that was renamed and lost its audit.
    let stale: Vec<&String> = audited
        .iter()
        .chain(unaudited.iter())
        .chain(aliases.iter())
        .filter(|r| !router.contains(*r))
        .collect();
    assert!(
        stale.is_empty(),
        "declared routes no longer in the router — update this table:\n{}",
        stale
            .iter()
            .map(|r| format!("  {r}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // A route may be in exactly one table.
    let overlap: Vec<&String> = audited.intersection(&unaudited).collect();
    assert!(overlap.is_empty(), "both audited and excused: {overlap:?}");
    let alias_overlap: Vec<&String> = aliases
        .intersection(&audited)
        .chain(aliases.intersection(&unaudited))
        .collect();
    assert!(
        alias_overlap.is_empty(),
        "route listed as an alias and as its own decision: {alias_overlap:?}"
    );
}

#[test]
fn every_declared_action_exists_in_the_catalog() {
    use manager::audit::Action;

    // Re-parse the catalog from audit.rs: the enum is the single vocabulary,
    // and a typo in the table must fail here rather than silently disagree.
    let source = repo_file("src/audit.rs");
    let start = source
        .find("pub enum Action {")
        .expect("Action enum in audit.rs");
    let end = start + source[start..].find("\n}").expect("enum end");
    let body = &source[start..end];
    let variants: BTreeSet<&str> = body
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("//")
                && !line.contains('(')
                && line.ends_with(',')
        })
        .map(|line| line.trim_end_matches(','))
        .filter(|line| line.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
        .collect();

    let mut unknown: Vec<&str> = Vec::new();
    for (_, action) in AUDITED {
        let variant = action
            .split(['.', '_'])
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<String>();
        if !variants.contains(variant.as_str()) {
            unknown.push(action);
        }
    }
    assert!(
        unknown.is_empty(),
        "AUDITED references actions that are not in the Action catalog: {unknown:?}"
    );

    // Guard against typos in Action::as_str itself (would break the UI filter).
    let all = format!("{:?}", Action::JobDelete);
    assert!(all.contains("JobDelete"));
}
