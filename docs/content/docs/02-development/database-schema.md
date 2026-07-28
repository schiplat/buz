---
title: Database Schema
weight: 2
description: "Buz Phase 1 database tables and indexes"
---

All tables use SQLite with WAL mode enabled, synchronous=NORMAL, and foreign_keys=ON.

## Tables

### runners

Execution node registry.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID v4 |
| name | TEXT UNIQUE | Human-readable name |
| register_token | TEXT | One-time registration token (cleared after use) |
| token | TEXT | Long-term communication token |
| labels | TEXT | JSON array, e.g. `["prod","linux-x64"]` |
| status | TEXT | `online`, `offline`, `busy` |
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

See [Users, Teams & RBAC](/docs/01-architecture/rbac/) for the full model.

| Table | Purpose |
|-------|---------|
| `users` | Login accounts (`password_hash` Argon2id, `is_platform_admin`) |
| `teams` | `name` + unique `slug` (replaces free-form `owner_group`) |
| `team_members` | `(team_id, user_id)` + role `owner\|maintainer\|developer\|viewer` |

### pipelines

Pipeline definitions.

| Column | Type | Notes |
|--------|------|-------|
| team_id | TEXT | **Required** team ownership (Phase 2) |
| trigger_type | TEXT | `manual`, `webhook`, or `cron` |
| concurrency_group | TEXT | Prevents concurrent execution of same pipeline |
| owner_group | TEXT | Namespace label for secret auto-access (Phase 1). Phase 2: replaced by `team_id` — see [RBAC](/docs/01-architecture/rbac/) |
| source | TEXT | `form` (UI) or `yaml` (file) |

### tasks

Pipeline step definitions.

| Column | Type | Notes |
|--------|------|-------|
| step_order | INT | Execution order |
| exec_type | TEXT | `local`, `ssh`, `docker`, or `wasm` |
| image | TEXT | Container image when `exec_type=docker` (empty otherwise) |
| fail_strategy | TEXT | `stop`, `continue`, or `ignore` |

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
| value | TEXT | Secret value |
| owner_group | TEXT | Phase 1 namespace; Phase 2 → `team_id` (required). See [RBAC](/docs/01-architecture/rbac/) |
| created_at, updated_at | TEXT | Auto-managed |

### pipeline_secrets

Many-to-many binding between pipelines and secrets. Pipelines receive secrets selected here at runtime.

| Column | Type | Notes |
|--------|------|-------|
| pipeline_id | TEXT FK → pipelines.id | ON DELETE CASCADE |
| secret_id | TEXT FK → secrets.id | ON DELETE CASCADE |
| UNIQUE(pipeline_id, secret_id) | | Prevents duplicate bindings |

Index: `idx_pipeline_secrets_pipeline` on `pipeline_id`.

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

### jobs

Job execution records. Includes `trigger_params` (JSON) for manual trigger inputs and `snapshot` (JSON) for the frozen pipeline definition at trigger time.

### job_logs

Chunked log storage. Unique constraint on `(job_id, chunk_index)` for idempotent uploads.
