---
title: Database Schema
weight: 2
description: "Buz Phase 1 database tables and indexes"
---

All tables use SQLite with WAL mode enabled, synchronous=NORMAL, and foreign_keys=ON.

## Tables

### runners

Runner registry.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID v4 |
| name | TEXT UNIQUE | Human-readable name |
| register_token | TEXT | One-time registration token (cleared after use) |
| token | TEXT | Long-term communication token |
| labels | TEXT | JSON array, e.g. `["prod","linux-x64"]` |
| status | TEXT | `online`, `offline`, `busy` |
| max_concurrent_jobs | INTEGER | Max parallel jobs (1–32). Default `1`. Configured in Manager UI |
| clean_workspace_after_job | INTEGER | Wipe job workspace after each job (`0`/`1`). UI-managed |
| shell_enabled | INTEGER | Allow shell steps (`0`/`1`, default `1`). UI-managed |
| ssh_enabled | INTEGER | Allow SSH steps (`0`/`1`, default `1`). UI-managed |
| docker_enabled | INTEGER | Allow Docker steps (`0`/`1`, default `0`). UI-managed |
| wasm_enabled | INTEGER | Allow WASM steps (`0`/`1`, default `0`). UI-managed |
| ssh_host_key_policy | TEXT | `insecure` \| `known_hosts`. UI-managed |
| last_seen | TEXT | ISO 8601 |
| created_at, updated_at | TEXT | Auto-managed |

### hosts

SSH remote host configuration. Stores encrypted credentials (private key or password) in `credential_data` when `credential_type` is `key_path` or `vault`. For `agent`, credentials are not stored — the runner uses its local SSH agent.

| Column | Type | Notes |
|--------|------|-------|
| credential_type | TEXT | `vault`, `key_path`, or `agent` |
| credential_data | TEXT | AES-256-GCM encrypted private key or password; empty for `agent` |
| team_id | TEXT NULL | Optional team scope; `NULL` = shared (platform-admin managed) |

### users / teams / team_members (Phase 2 RBAC)

See [Users, Teams & RBAC](/docs/01-architecture/platform/rbac/) for the full model.

| Table | Purpose |
|-------|---------|
| `users` | Login accounts (`password_hash` Argon2id, `is_platform_admin`, `is_super_admin`) |
| `teams` | `name` + unique `slug` (replaces free-form `owner_group`) |
| `team_members` | `(team_id, user_id)` + role `owner\|maintainer\|developer\|viewer` |

### pipelines

Runtime **projection** of pipeline definitions. For `source=yaml`, the file under Manager `yaml_scan_dir` is authoritative — see [YAML-First Authoring](/docs/01-architecture/authoring/yaml-first-authoring/).

| Column | Type | Notes |
|--------|------|-------|
| team_id | TEXT | **Required** team ownership (Phase 2) |
| trigger_type | TEXT | `manual`, `webhook`, or `cron` |
| concurrency_group | TEXT | Prevents concurrent execution of same pipeline |
| owner_group | TEXT | Team slug mirror (Phase 1 namespace). Phase 2: ownership via `team_id` — see [RBAC](/docs/01-architecture/platform/rbac/) |
| source | TEXT | Prefer `yaml` (file authority). Legacy `form` until migrated |
| yaml_path | TEXT | **Basename only** (e.g. `demo.yml`) when `source=yaml`; flat scan dir — no subdirectories |
| is_active | INTEGER | Toggle sticks across scanner upserts (not forced back to `1`) |

### tasks

Pipeline step definitions.

| Column | Type | Notes |
|--------|------|-------|
| step_order | INT | Execution order |
| exec_type | TEXT | `local`, `ssh`, `docker`, or `wasm` |
| image | TEXT | Container image when `exec_type=docker` (empty otherwise) |
| registry_secret | TEXT | Manager Secret name for `docker login` (shell / SSH / docker; empty otherwise) |
| fail_strategy | TEXT | `stop`, `continue`, or `ignore` |
| hosts_strategy | TEXT | SSH multi-host fan-out: `chain` (default) or `parallel` |

### variables

Pipeline-scoped environment variables defined in YAML (`env:` block). Each variable is linked to one pipeline.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID v4 |
| pipeline_id | TEXT FK → pipelines.id | |
| var_name | TEXT | Variable name |
| source_type | TEXT | `inline` (from YAML `env:`) |
| is_secret | INT | 0 (non-secret) |

### platform_vars

Platform-level non-sensitive variables managed in the UI (Infrastructure → Variables). Used for configuration shared across pipelines.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID v4 |
| name | TEXT UNIQUE | Variable name |
| value | TEXT | Variable value |
| updated_at | TEXT | Auto-managed |

### pipeline_vars

Many-to-many binding between pipelines and platform variables.

| Column | Type | Notes |
|--------|------|-------|
| pipeline_id | TEXT FK → pipelines.id | ON DELETE CASCADE |
| var_id | TEXT FK → platform_vars.id | ON DELETE CASCADE |
| UNIQUE(pipeline_id, var_id) | | Prevents duplicate bindings |

Index: `idx_pipeline_vars_pipeline` on `pipeline_id`.

### secrets

Secret storage. Values are stored in the database and injected as environment variables by the runner at dispatch time. Values are **never logged**.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID v4 |
| name | TEXT UNIQUE | Secret name (e.g. `DOCKER_PASS`, `GIT_SSH_KEY`) |
| value | TEXT | AES-256-GCM ciphertext only (plaintext rows are rejected at decrypt) |
| owner_group | TEXT | Team slug mirror; Phase 2 ownership via `team_id` (required). See [RBAC](/docs/01-architecture/platform/rbac/) |
| team_id | TEXT | Owning team (required after bootstrap) |
| team_shared | INTEGER NOT NULL DEFAULT 0 | `1` = same-team pipelines auto-receive this secret at job claim; `0` = explicit bind/reference only. See [Secrets Access Control](/docs/01-architecture/platform/secrets-access-control/) |
| kind | TEXT NOT NULL DEFAULT `'generic'` | `generic` = env inject; `registry` = JSON credentials for `registry_secret` / docker login. Backfill marks decryptable registry JSON as `registry`. |
| created_at, updated_at | TEXT | Auto-managed |

### pipeline_secrets

Many-to-many binding between pipelines and secrets. Pipelines receive secrets selected here at runtime.

| Column | Type | Notes |
|--------|------|-------|
| pipeline_id | TEXT FK → pipelines.id | ON DELETE CASCADE |
| secret_id | TEXT FK → secrets.id | ON DELETE CASCADE |
| UNIQUE(pipeline_id, secret_id) | | Prevents duplicate bindings |

Index: `idx_pipeline_secrets_pipeline` on `pipeline_id`.

### pipeline_git_hooks

GitHub/Gitee webhook adapter config per pipeline. See [Git Webhook Triggers](/docs/01-architecture/authoring/git-webhook-triggers/).

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID |
| pipeline_id | TEXT FK → pipelines.id | Many hooks per pipeline |
| label | TEXT | Optional display name |
| provider | TEXT | `github` \| `gitee` |
| hook_key | TEXT UNIQUE | Opaque URL path segment |
| secret_ciphertext | TEXT | AES-256-GCM; plaintext shown once in UI; pasted into platform Webhook Secret |
| branch_filter | TEXT | Optional; empty = all branches |
| enabled | INTEGER | 0/1 |
| created_at, updated_at | TEXT | |

### pipeline_trigger_events

Unified trigger timeline for Pipeline detail → **Trigger events**.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID |
| pipeline_id | TEXT FK | |
| source | TEXT | `git` \| `webhook` \| `manual` \| `cron` |
| provider | TEXT | nullable (`github` / `gitee`) |
| event_type | TEXT | `push`, `ping`, … |
| delivery_id | TEXT | Platform delivery id (idempotency) |
| repo_full_name, git_ref, branch, tag | TEXT | |
| commit_sha, commit_message, sender | TEXT | |
| compare_url | TEXT | |
| received_at | TEXT | |
| decision | TEXT | `triggered` \| `skipped` \| `rejected` \| `failed` |
| skip_reason | TEXT | |
| job_id | TEXT | nullable FK → jobs |
| created_at | TEXT | |

Partial unique index on `(pipeline_id, delivery_id)` when delivery_id is set.

### github_connections

GitHub API connection credentials for Actions sync (PAT, optional GHE base URL).
See [GitHub Actions Secrets/Variables Sync](/docs/01-architecture/platform/github-actions-sync/).

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID v4 |
| name | TEXT UNIQUE | Human-readable name, e.g. `buz-org-writer` |
| api_base_url | TEXT | Default `https://api.github.com`; GHE server base URL supported |
| token_ciphertext | TEXT | AES-256-GCM ciphertext (`crypto::encrypt(token, jwt_secret)`); never returned by API |
| fetch_owners | TEXT | Comma-separated org/user owners to auto-fetch; empty string disables |
| fetch_interval_minutes | INTEGER NULL | Poll interval; NULL disables auto-fetch. Must be > 0 when set |
| last_fetch_at | TEXT NULL | Last successful fetch (manual or auto); drives the scheduler's due check |
| created_at, updated_at | TEXT | Auto-managed |

### github_repos

Pre-registered target repos, reused across secret/variable binds.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID v4 |
| connection_id | TEXT FK → github_connections.id | ON DELETE CASCADE |
| owner | TEXT | Org or user name |
| repo | TEXT | Repo name |
| created_at, updated_at | TEXT | Auto-managed |
| UNIQUE(connection_id, owner, repo) | | Prevents duplicates per connection |

Index: `idx_github_repos_connection` on `connection_id`.

### secret_github_binds / variable_github_binds

Many-to-many binding between a Buz secret (resp. platform var) and a GitHub
repo, plus the last operation state.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID v4 |
| secret_id / var_id | TEXT FK | `secrets.id` / `platform_vars.id`, ON DELETE CASCADE |
| repo_id | TEXT FK → github_repos.id | ON DELETE CASCADE |
| last_pushed_at | TEXT NULL | Last successful push time |
| last_status | TEXT | `pending` \| `ok` \| `failed` |
| last_error | TEXT | Failure message; empty when ok |
| last_op | TEXT | Last operation: `bind` (added) \| `push` (value pushed) \| `unbind` (remote cleanup failed, row kept) |
| override_value | TEXT NULL | Per-repo Actions value when pushing. **Variables**: plaintext; `NULL` = inherit `platform_vars.value`. **Secrets**: AES-256-GCM ciphertext (same scheme as `secrets.value`); `NULL` = inherit secret default. API never returns secret override plaintext (`has_override` only). |
| created_at, updated_at | TEXT | Auto-managed |
| UNIQUE(secret_id, repo_id) / UNIQUE(var_id, repo_id) | | One bind per resource per repo |

Indexes: `idx_secret_github_binds_secret` on `secret_id`,
`idx_variable_github_binds_var` on `var_id`.

### audit_logs

Platform-admin operation audit. Written best-effort after successful JWT mutations (and `auth.login`). JSON columns are **already redacted** before insert.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID |
| created_at | TEXT | |
| actor_user_id | TEXT | |
| actor_username | TEXT | |
| action | TEXT | e.g. `secret.create`, `pipeline.trigger` |
| resource_type | TEXT | e.g. `secret`, `pipeline` |
| resource_id | TEXT | |
| resource_name | TEXT | Display name at write time |
| team_id | TEXT NULL | |
| before_json | TEXT NULL | Redacted JSON snapshot |
| after_json | TEXT NULL | Redacted JSON snapshot |
| meta_json | TEXT NULL | Optional extras |
| ip | TEXT | Optional; often empty |

Indexes: `(created_at DESC)`, `(action)`, `(actor_username)`, `(resource_type, resource_id)`.

### api_tokens

Long-lived bearer credentials that authenticate like a user JWT. Created by platform admins in **Access → API tokens** (`/api-tokens`).

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID |
| user_id | TEXT FK → users.id | Owning admin (ON DELETE CASCADE) |
| name | TEXT | Display label, `[A-Za-z0-9._-]`, ≤ 64 chars |
| token_hash | TEXT | **SHA-256** of the token — plaintext is never stored |
| token_prefix | TEXT | Display prefix e.g. `buz_a1b2c3…` |
| created_at | TEXT | |
| last_used_at | TEXT NULL | Touched on successful auth |
| revoked_at | TEXT NULL | Set on revoke; revoked tokens no longer authenticate |

Indexes: `(user_id)`.

### runner_events

Runner lifecycle event log. Records registration, heartbeat, freeze/unfreeze, and status changes.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID v4 |
| runner_id | TEXT FK → runners.id | |
| event_type | TEXT | `registered`, `heartbeat`, `frozen`, `unfrozen`, `offline`, etc. |
| event_data | TEXT | JSON payload with event details |
| created_at | TEXT | Auto-managed |

Index: `idx_runner_events_runner` on `runner_id`.

### environments / environment_var_overrides / environment_secret_overrides

Named platform scopes (`staging`, `production`, …) that overlay **bound** Variable
and **allowlisted** Secret defaults at job claim when the operator selects an
Environment at Trigger.

| Column | Notes |
|--------|--------|
| `environments.id/name/slug` | `slug` unique; lowercase letters, digits, `-`, `_` |
| `environments.protection_enabled` | INTEGER 0/1 — approval gate |
| `environments.min_trigger_role` / `min_approve_role` | owner \| maintainer \| developer |
| `environments.require_approvals` | ≥ 1 |
| `environments.forbid_self_approve` | INTEGER 0/1 |
| `environments.approval_timeout_seconds` | default 86400 |
| `environment_var_overrides.(environment_id, var_id)` | PK; `value` plaintext |
| `environment_secret_overrides.(environment_id, secret_id)` | PK; `value` AES ciphertext (never returned by API) |
| `jobs.environment_id` | nullable FK → `environments(id)` ON DELETE SET NULL |
| `jobs.trigger_user_id` | nullable; set for manual triggers (self-approve checks) |
| `job_approvals` | Append-only: `approve` \| `reject` \| `cancel` \| `timeout`; stores `username`; UNIQUE `(job_id, user_id, action)` |

Claim merge: platform vars → env var overrides (same keys) → pipeline `env:` →
inputs; secrets allowlist → env secret overrides (same names). See
[Environments](/docs/01-architecture/platform/environments/) and
[Environment Approval Gates](/docs/01-architecture/platform/environment-approvals/).

### jobs

Job execution records. Includes `trigger_params` (JSON) for manual trigger inputs, `snapshot` (JSON) for the frozen pipeline definition at trigger time (may include `environment` slug, `protection`, `approval`), optional `environment_id`, and `trigger_user_id`. Status may be `waiting_approval` until Environment approval gates pass. `GET /jobs/:id` returns `approvals[]` from `job_approvals`.

### job_logs

Chunked log storage. Unique constraint on `(job_id, chunk_index)` for idempotent uploads.

Reads use `ORDER BY step_order, chunk_index`. Per-step index layout is defined by the Runner (`step_log_base` + offsets in `crates/runner/src/client/poll.rs`) so display order matches execution: prepare → config → config prep → login → exec → RUN → stream → result. See [Secrets Access Control — Log display order](../../01-architecture/secrets-access-control.md#log-display-order-chunk_index).
