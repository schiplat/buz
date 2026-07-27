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

SSH remote host configuration. Stores credential references (not plaintext passwords).

| Column | Type | Notes |
|--------|------|-------|
| credential_type | TEXT | `vault`, `key_path`, or `agent` |
| credential_ref | TEXT | Vault path, local key path, or "ssh-agent" |

### pipelines

Pipeline definitions.

| Column | Type | Notes |
|--------|------|-------|
| trigger_type | TEXT | `manual`, `webhook`, or `cron` |
| concurrency_group | TEXT | Prevents concurrent execution of same pipeline |
| source | TEXT | `form` (UI) or `yaml` (file) |

### tasks

Pipeline step definitions.

| Column | Type | Notes |
|--------|------|-------|
| step_order | INT | Execution order |
| exec_type | TEXT | `local`, `ssh`, `docker`, or `wasm` |
| fail_strategy | TEXT | `stop`, `continue`, or `ignore` |

### variables

Environment variables and secret bindings. Secrets never store values — only Vault paths.

### jobs

Job execution records. Includes `trigger_params` (JSON) for manual trigger inputs.

### job_logs

Chunked log storage. Unique constraint on `(job_id, chunk_index)` for idempotent uploads.
