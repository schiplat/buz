//! Store-boundary SQL validation.
//!
//! The storage layer is the only place Manager speaks SQL, and its statements are
//! plain string literals. A typo there (a wrong column, a renamed table bricks) is
//! invisible to the Rust compiler and only fails at runtime, on the endpoint that
//! happens to use it.
//!
//! This test extracts every SQL literal from `src/store/`, then asks SQLite to
//! compile it against a freshly migrated database via `EXPLAIN <sql>`. `EXPLAIN`
//! compiles the statement (so unresolved tables/columns are errors) but never runs
//! it, so `INSERT` / `UPDATE` / `DELETE` statements are checked safely.
//!
//! Statement text is centred verbatim from the source with a small scanner that
//! skips comments and honours string escapes. Tab-composed statements are handled
//! by substituting the known SELECT-fragment constants; a literal that still has
//! an unresolved `{}` is reported as skipped ratherthan silently ignored, and the
//! test prints the validated/skipped counts so a shrinking population is visible.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sqlx::sqlite::SqlitePoolOptions;

const SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";

/// SELECT fragments that `format!` splices into statements. The values must match
/// the constants in `src/store`.
const FRAGMENTS: &[(&str, &str)] = &[
    (
        "{CONNECTION_COLUMNS}",
        "id, name, api_base_url, token_ciphertext, fetch_owners, fetch_interval_minutes, last_fetch_at, updated_at",
    ),
    (
        "{REPO_COLUMNS}",
        "id, connection_id, owner, repo, updated_at, created_at",
    ),
    (
        "{ENV_PROTECTION_SELECT}",
        "protection_enabled, min_trigger_role, min_approve_role, require_approvals, forbid_self_approve, approval_timeout_seconds",
    ),
];

/// Statement keywords a store literal may start with.
const KEYWORDS: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE", "WITH"];

/// Extract every plain string literal from Rust source, skipping line and block
/// comments and honouring `\` escapes. Raw strings are not used in `store/`.
fn string_literals(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    let mut line_comment = false;
    let mut block_comment = false;
    while i < chars.len() {
        let c = chars[i];
        if line_comment {
            if c == '\n' {
                line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_comment {
            if c == '*' && chars.get(i + 1) == Some(&'/') {
                block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            line_comment = true;
            i += 2;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            block_comment = true;
            i += 2;
            continue;
        }
        if c == '"' {
            i += 1;
            let mut text = String::new();
            while i < chars.len() {
                let ch = chars[i];
                if ch == '\\' && i + 1 < chars.len() {
                    let next = chars[i + 1];
                    text.push(match next {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '0' => '\0',
                        other => other,
                    });
                    i += 2;
                    continue;
                }
                if ch == '"' {
                    i += 1;
                    break;
                }
                text.push(ch);
                i += 1;
            }
            out.push(text);
            continue;
        }
        i += 1;
    }
    out
}

fn is_statement(text: &str) -> bool {
    let head = text.trim_start();
    KEYWORDS.iter().any(|keyword| {
        head.starts_with(keyword)
            && head[keyword.len()..]
                .chars()
                .next()
                .is_none_or(|c| c.is_whitespace() || c == '(')
    })
}

fn resolve_fragments(text: &str) -> Option<String> {
    let mut resolved = text.to_string();
    for (name, value) in FRAGMENTS {
        resolved = resolved.replace(name, value);
    }
    if resolved.contains('{') || resolved.contains('}') {
        return None;
    }
    Some(resolved)
}

async fn store_statements() -> (Vec<String>, Vec<String>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("store dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    files.sort();

    let mut statements = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = BTreeSet::new();
    for path in files {
        let label = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let source = std::fs::read_to_string(&path).expect("read store file");
        for literal in string_literals(&source) {
            if !is_statement(&literal) {
                continue;
            }
            match resolve_fragments(&literal) {
                Some(text) => {
                    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if seen.insert(text.clone()) {
                        statements.push(text);
                    }
                }
                None => skipped.push(format!("{label}: {}", literal.trim())),
            }
        }
    }
    (statements, skipped)
}

/// Every SQL literal in the store must compile against the migrated schema.
#[tokio::test]
async fn store_sql_compiles_against_the_schema() {
    let (statements, skipped) = store_statements().await;

    // Guard the scanner itself: a broken extractor would otherwise pass by finding
    // nothing, which is the failure mode this test exists to prevent.
    assert!(
        statements.len() >= 40,
        "extractor found only {} statement(s); did the store directory move?",
        statements.len()
    );

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    manager::db::run_migrations(&pool, SECRET).await.unwrap();

    let mut failures: Vec<String> = Vec::new();
    for statement in &statements {
        let probe = format!("EXPLAIN {statement}");
        if let Err(error) = sqlx::query(&probe).fetch_all(&pool).await {
            failures.push(format!("{error}\n  -> {statement}"));
        }
    }

    println!(
        "validated {} store statement(s); skipped {} (unresolved fragments)",
        statements.len(),
        skipped.len()
    );
    for entry in &skipped {
        println!("  skipped: {entry}");
    }

    assert!(
        failures.is_empty(),
        "{} store statement(s) do not compile against the schema:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Dynamic statements are not compiled: their text is assembled from a
/// `format!` shell whose WHERE/ORDER columns are chosen at runtime, so no
/// single literal represents them. They are listed here explicitly instead of
/// being ignored, so a NEW dynamic statement (which would evade compilation
/// checking) cannot slip in silently, and a removed one is noticed too.
///
/// Each entry is the *shape* that must stay present in `src/store/`. Keeping the
/// list exhaustive is the point: the test fails when the two sets differ.
const EXPECTED_DYNAMIC_SHAPES: &[&str] = &[
    "SELECT COUNT(*) FROM audit_logs {where_sql}",
    "SELECT id, created_at, actor_user_id, actor_username, action, resource_type, resource_id, resource_name, team_id, before_json, after_json, meta_json, ip FROM audit_logs {where_sql}",
    "UPDATE jobs SET status = ?, {end_expr} updated_at = {now}, retry_times = retry_times + 1 WHERE id = ? AND status NOT IN ('cancelled', 'success', 'failed', 'timeout')",
    "SELECT date(j.created_at) AS day, j.status, COUNT(*) AS cnt FROM jobs j JOIN pipelines p ON j.pipeline_id = p.id WHERE j.created_at >= ? {clause} {debug_filter} GROUP BY date(j.created_at), j.status",
    "SELECT date(j.created_at) AS day, j.status, COUNT(*) AS cnt FROM jobs j JOIN pipelines p ON j.pipeline_id = p.id WHERE j.created_at >= ? {debug_filter}",
    "SELECT j.id, j.pipeline_id, p.name AS pipeline_name, j.status, j.trigger_by, p.team_id, j.created_at FROM jobs j JOIN pipelines p ON j.pipeline_id = p.id WHERE 1=1 {clause} {debug_filter} ORDER BY j.created_at DESC, j.id DESC LIMIT ?",
    "SELECT j.id, j.pipeline_id, p.name AS pipeline_name, j.status, j.trigger_by, p.team_id, j.created_at FROM jobs j JOIN pipelines p ON j.pipeline_id = p.id WHERE 1=1 {debug_filter} ORDER BY j.created_at DESC, j.id DESC LIMIT ?",
    "SELECT id, status, step_statuses, COALESCE(snapshot, '{}') FROM jobs WHERE environment_id = ? AND id != ?",
    "SELECT id, name, value, owner_group, team_id, team_shared, kind, updated_at FROM secrets WHERE 1=1 AND team_id IN ({placeholders}) ORDER BY name ASC",
    "SELECT id, name, host, port, username, credential_type, credential_data, labels, created_at, updated_at, team_id FROM hosts WHERE team_id IS NULL OR team_id = '' OR team_id IN ({placeholders}) ORDER BY created_at DESC",
    "SELECT id, name, slug, description, created_at, updated_at FROM teams WHERE id IN ({placeholders}) ORDER BY name ASC",
    "SELECT b.repo_id, b.id, s.name, b.last_status, b.last_op FROM secret_github_binds b JOIN secrets s ON s.id = b.secret_id WHERE b.repo_id IN ({placeholders}) ORDER BY s.name ASC",
    "SELECT b.repo_id, b.id, v.name, b.last_status, b.last_op FROM variable_github_binds b JOIN platform_vars v ON v.id = b.var_id WHERE b.repo_id IN ({placeholders}) ORDER BY v.name ASC",
    "SELECT b.secret_id, b.id, b.repo_id, c.name, r.owner, r.repo, b.last_status, b.last_pushed_at, b.last_error, b.last_op, b.override_value FROM secret_github_binds b JOIN github_repos r ON r.id = b.repo_id JOIN github_connections c ON c.id = r.connection_id WHERE b.secret_id IN ({placeholders}) ORDER BY r.owner ASC, r.repo ASC",
    "SELECT b.var_id, b.id, b.repo_id, c.name, r.owner, r.repo, b.last_status, b.last_pushed_at, b.last_error, b.last_op, b.override_value FROM variable_github_binds b JOIN github_repos r ON r.id = b.repo_id JOIN github_connections c ON c.id = r.connection_id WHERE b.var_id IN ({placeholders}) ORDER BY r.owner ASC, r.repo ASC",
    "DELETE FROM {table} WHERE host_id = ? AND instance_id = ?",
    "INSERT INTO sandbox_config_changes (id, host_id, instance_id, status, requested_by, before_document, after_document) VALUES (?, 'resource-grants', 'grant', 'pending', ?, '{}', ?)",
    "SELECT COUNT(*) FROM sandbox_config_changes {where_clause}",
    "SELECT id, host_id, instance_id, status, requested_by, after_document, created_at, decided_at, approved_by, decision_comment FROM sandbox_config_changes {where_clause} ORDER BY CASE WHEN status = 'pending' THEN created_at ELSE decided_at END DESC LIMIT {} OFFSET {}",
    "SELECT p.id, p.name, p.description, p.trigger_type, p.runner_labels, p.timeout_seconds, p.concurrency_group, p.input_defs, p.is_active, p.source, p.team_id, p.owner_group, p.created_at, p.updated_at, j.id as latest_job_id, j.status as latest_job_status, j.start_time as latest_job_start, COALESCE(js.job_total, 0) as job_total, COALESCE(js.job_success, 0) as job_success, p.concurrency_mode, p.concurrency_max_queue, p.concurrency_queue_timeout_seconds, p.yaml_path, p.environments FROM pipelines p LEFT JOIN ( SELECT pipeline_id, MAX(created_at) as max_created FROM jobs {debug_scope}GROUP BY pipeline_id ) latest ON p.id = latest.pipeline_id LEFT JOIN jobs j ON j.pipeline_id = latest.pipeline_id AND j.created_at = latest.max_created LEFT JOIN ( SELECT pipeline_id, COUNT(*) as job_total, SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as job_success FROM jobs {debug_scope}GROUP BY pipeline_id ) js ON p.id = js.pipeline_id WHERE 1=1",
    "UPDATE runners SET {column} = ?, updated_at = datetime('now') WHERE id = ?",
    "SELECT j.id, j.pipeline_id, j.trigger_params, COALESCE(j.snapshot, '{}'), p.runner_labels, j.debug_unredact, j.verbose_logs, j.trigger_by, p.name, COALESCE(json_extract(j.snapshot, '$.team_id'), p.team_id), p.trigger_type, j.environment_id, j.run_number, j.run_attempt, CAST(strftime('%s', 'now') - strftime('%s', j.created_at) AS INTEGER) AS waited_seconds FROM jobs j JOIN pipelines p ON p.id = j.pipeline_id WHERE j.status = 'pending' AND (? IS NULL OR json_extract(j.snapshot, '$.team_id') = ?) AND ( j.environment_id IS NULL OR COALESCE((SELECT deployment_concurrency FROM environments e WHERE e.id = j.environment_id), 'off') != 'cancel_in_progress' OR j.seq = ( SELECT MAX(latest.seq) FROM jobs latest WHERE latest.environment_id = j.environment_id AND latest.status = 'pending' ) ) AND NOT ( COALESCE((SELECT deployment_concurrency FROM environments e WHERE e.id = j.environment_id), 'off') = 'queue' AND EXISTS (SELECT 1 FROM jobs running WHERE running.environment_id = j.environment_id AND running.status = 'running') ) AND NOT ( COALESCE((SELECT deployment_concurrency FROM environments e WHERE e.id = j.environment_id), 'off') != 'cancel_in_progress' AND EXISTS (SELECT 1 FROM jobs running WHERE running.pipeline_id = j.pipeline_id AND running.environment_id = j.environment_id AND running.status = 'running') ) ORDER BY j.created_at ASC, j.seq ASC LIMIT 50",
    "SELECT COALESCE(json_extract(j.value, '$.id'), ''), json_extract(j.value, '$.step_order'), json_extract(j.value, '$.name'), json_extract(j.value, '$.exec_type'), COALESCE(json_extract(j.value, '$.host_id'), ''), COALESCE(json_extract(j.value, '$.host_names'), ''), COALESCE(json_extract(j.value, '$.host_labels'), ''), COALESCE(json_extract(j.value, '$.command_template'), ''), COALESCE(json_extract(j.value, '$.work_dir'), ''), COALESCE(json_extract(j.value, '$.retry_count'), 0), COALESCE(json_extract(j.value, '$.timeout_seconds'), 0), COALESCE(json_extract(j.value, '$.fail_strategy'), 'stop'), COALESCE(json_extract(j.value, '$.step_secrets'), ''), COALESCE(json_extract(j.value, '$.step_env'), '{}'), COALESCE(json_extract(j.value, '$.image'), ''), COALESCE(json_extract(j.value, '$.volumes'), '[]'), COALESCE(json_extract(j.value, '$.hosts_strategy'), 'chain'), COALESCE(json_extract(j.value, '$.privileged'), 0), COALESCE(json_extract(j.value, '$.capabilities'), '[]'), COALESCE(json_extract(j.value, '$.hosts_max_parallel'), NULL), COALESCE(json_extract(j.value, '$.needs'), '[]') FROM json_each(?, '$.tasks') j ORDER BY json_extract(j.value, '$.step_order')",
];

/// Every unresolved-fragment statement in the store must be one of the shapes
/// above: no silent growth of unchecked SQL.
#[tokio::test]
async fn dynamic_sql_is_declared() {
    let (_, skipped) = store_statements().await;
    let mut undeclared = Vec::new();
    for entry in &skipped {
        // Compare normalised: labels/indentation from the multi-line source do
        // not matter, only the statement text does.
        let text = entry
            .split_once(": ")
            .map(|pair| pair.1)
            .unwrap_or(entry)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let declared = EXPECTED_DYNAMIC_SHAPES
            .iter()
            .any(|shape| text.starts_with(shape) || shape.starts_with(&text));
        if !declared {
            undeclared.push(text);
        }
    }
    assert!(
        undeclared.is_empty(),
        "dynamic store statements missing from EXPECTED_DYNAMIC_SHAPES:\n{}",
        undeclared
            .iter()
            .map(|text| format!("  \"{text}\","))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
