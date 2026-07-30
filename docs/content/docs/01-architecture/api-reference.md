---
title: API Reference
weight: 4
description: "Buz Manager REST API reference"
---

All API endpoints use the prefix `/api/v1/`.

## Response Format

```json
{
  "code": 0,
  "message": "success",
  "data": {}
}
```

### Error Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1001 | Authentication failed |
| 1002 | Token expired |
| 1003 | Insufficient permissions |
| 2001 | Resource not found |
| 2002 | Resource conflict |
| 3001 | Validation failed |
| 5000 | Internal server error |

## Authentication

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/auth/setup-status` | None |
| POST | `/api/v1/auth/setup` | None (only when no users) |
| POST | `/api/v1/auth/login` | None |
| GET | `/api/v1/auth/me` | JWT |
| GET | `/api/v1/auth/verify` | JWT |
| POST | `/api/v1/auth/change-password` | JWT |

### GET /auth/setup-status

```json
{ "code": 0, "data": { "needs_setup": true } }
```

`needs_setup` is `true` when the `users` table is empty. The Web UI redirects to `/setup`.

### POST /auth/setup

Create the first platform admin (rejected with conflict if any user already exists). Returns a login JWT (same shape as login).

Request:
```json
{ "username": "admin", "password": "at-least-8-chars" }
```

Rules: username 2–64 chars (`[A-Za-z0-9_.-]`); password ≥ 8 characters and must include **uppercase**, **lowercase**, a **digit**, and a **symbol** (no whitespace). Stored as **Argon2id** (not AES — AES is only for Infrastructure Secrets/Hosts).

### POST /auth/login

Request:
```json
{ "username": "admin", "password": "your-password" }
```

Username is trimmed server-side. Password is verified with Argon2id against `users.password_hash`.

Response:
```json
{ "code": 0, "data": { "token": "eyJ...", "user_id": "…", "username": "admin", "is_platform_admin": true } }
```

If the instance is not initialized, login fails and the client should open `/setup`.

### POST /auth/change-password

Authenticated user updates their own password. Requires the current password.

Request:
```json
{
  "current_password": "old-secret",
  "new_password": "new-secret"
}
```

Rules: both fields required; `new_password` ≥ 6 characters and must differ from `current_password`. Wrong current password → validation error (does not invalidate the session).

## Version & Health

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/health` | None |

### GET /health

Returns liveness plus package version (no separate `/version` endpoint).

```json
{ "status": "ok", "name": "Buz", "version": "0.2.0", "component": "manager" }
```

Workspace version is defined in root `Cargo.toml` (`[workspace.package].version`) and mirrored in `frontend/package.json`. CLI: `buz-manager --version` / `buz-runner --version`.

## Runners

> Policy fields (`max_concurrent_jobs`, engines, cleanup, `ssh_host_key_policy`) are UI-managed. See [Runner Policy](/docs/01-architecture/runner-concurrent-jobs/).

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/runners` | Admin JWT |
| GET | `/api/v1/runners/:id` | Admin JWT |
| POST | `/api/v1/runners` | Admin JWT |
| PUT | `/api/v1/runners/:id` | Admin JWT |
| DELETE | `/api/v1/runners/:id` | Admin JWT |
| PATCH | `/api/v1/runners/:id/freeze` | Admin JWT |
| GET | `/api/v1/runners/:id/events` | Admin JWT |
| POST | `/api/v1/runners/register` | Register Token |
| POST | `/api/v1/runners/heartbeat` | Runner JWT |
| GET | `/api/v1/runners/poll` | Runner JWT |

### POST /runners — Create Runner (Admin)

Request:
```json
{
  "name": "prod-runner-01",
  "labels": ["linux-x64", "prod"],
  "version": "0.2.0",
  "max_concurrent_jobs": 3,
  "clean_workspace_after_job": true,
  "shell_enabled": true,
  "ssh_enabled": true,
  "docker_enabled": true,
  "wasm_enabled": false,
  "ssh_host_key_policy": "insecure"
}
```

Policy fields are optional. Defaults: `max_concurrent_jobs=1`, cleanup off, shell+ssh on, docker+wasm off, `ssh_host_key_policy=insecure`.

Response:
```json
{
  "code": 0,
  "data": {
    "id": "uuid",
    "name": "prod-runner-01",
    "register_token": "one-time-token",
    "max_concurrent_jobs": 3,
    "policy": { "max_concurrent_jobs": 3, "docker_enabled": true }
  }
}
```

### PUT /runners/:id — Update Runner (Admin)

Request (any field optional):
```json
{
  "labels": ["linux-x64", "docker"],
  "max_concurrent_jobs": 4,
  "docker_enabled": true,
  "ssh_host_key_policy": "known_hosts"
}
```

Returns the updated runner object (includes `running_jobs` and all policy fields).

List/get responses include policy columns (`max_concurrent_jobs`, `clean_workspace_after_job`, engine flags, `ssh_host_key_policy`) and `running_jobs`.

### POST /runners/register — Runner Registration

Request:
```json
{
  "name": "prod-runner-01",
  "register_token": "one-time-token",
  "labels": ["linux-x64"],
  "version": "0.2.0"
}
```

Response:
```json
{
  "code": 0,
  "data": {
    "runner_id": "uuid",
    "token": "jwt",
    "policy": {
      "max_concurrent_jobs": 1,
      "clean_workspace_after_job": false,
      "shell_enabled": true,
      "ssh_enabled": true,
      "docker_enabled": false,
      "wasm_enabled": false,
      "ssh_host_key_policy": "insecure"
    }
  }
}
```

### POST /runners/heartbeat — Runner Heartbeat

Request (Bearer token):
```json
{ "status": "online" }
```

Response includes the current UI policy snapshot (Runner refreshes concurrency, engines, cleanup):
```json
{
  "code": 0,
  "data": {
    "policy": {
      "max_concurrent_jobs": 3,
      "clean_workspace_after_job": true,
      "shell_enabled": true,
      "ssh_enabled": true,
      "docker_enabled": true,
      "wasm_enabled": false,
      "ssh_host_key_policy": "insecure"
    }
  }
}
```

### GET /runners/poll — Runner Task Poll

Before claiming a job, Manager checks that
`COUNT(jobs WHERE runner_id=? AND status='running') < max_concurrent_jobs`.
If the runner is at capacity, poll returns `data: null`.

Response (with task):
```json
{
  "code": 0,
  "data": {
    "job_id": "uuid",
    "pipeline_id": "uuid",
    "tasks": [
      {
        "id": "uuid",
        "step_order": 0,
        "name": "build",
        "exec_type": "shell",
        "command_template": "npm run build",
        "work_dir": "",
        "step_secrets": {},
        "registry_secret": "",
        "registry_auth": null
      }
    ],
    "inputs": { "tag": "1.0" },
    "env": {
      "REGISTRY": "registry.example.com",
      "APP_NAME": "myapp"
    },
    "secrets": {
      "DOCKER_PASS": "hunter2"
    }
  }
}
```

- `env` — platform Variables + pipeline inline `env:` (shown in clear in Job logs)
- `secrets` — Infrastructure Secrets only (redacted as `***` in RUN banners and stream logs)
- `inputs` — trigger params (also not redacted; Runner also exports `INPUT_<KEY>` uppercased)
- Per-step `registry_auth` — resolved from `registry_secret` for `docker login` only (not injected as `$NAME`)

Empty response: `204 No Content`

## Pipelines

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/pipelines` | Admin JWT |
| POST | `/api/v1/pipelines` | Admin JWT |
| POST | `/api/v1/pipelines/validate` | Admin JWT |
| GET | `/api/v1/pipelines/:id` | Admin JWT |
| PUT | `/api/v1/pipelines/:id` | Admin JWT |
| DELETE | `/api/v1/pipelines/:id` | Admin JWT |
| PATCH | `/api/v1/pipelines/:id/toggle-active` | Admin JWT |
| POST | `/api/v1/pipelines/:id/trigger` | Admin JWT |

### GET /pipelines — List

Each item includes latest-job summary and **build success rate**:

| Field | Type | Description |
|-------|------|-------------|
| `job_total` | number | Total jobs (builds) for this pipeline |
| `job_success` | number | Jobs with `status = success` |
| `success_rate` | number \| null | `job_success / job_total` (0–1). `null` when `job_total` is 0 |

UI column: **Rate** (percentage + `success/total` hint).

### POST /pipelines — Create

Request:
```json
{
  "name": "hello-world",
  "description": "A simple test pipeline",
  "trigger_type": "manual",
  "runner_labels": ["linux-x64"],
  "timeout_seconds": 3600,
  "concurrency_group": null,
  "steps": [
    { "name": "echo-step", "run": "echo Hello from Buz" },
    { "name": "build", "run": "npm run build", "dir": "./frontend" }
  ]
}
```

### POST /pipelines/:id/trigger — Trigger Run

Response:
```json
{
  "code": 0,
  "data": { "job_id": "uuid" }
}
```

## Jobs

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/jobs` | JWT |
| GET | `/api/v1/jobs/:id` | JWT |
| POST | `/api/v1/jobs/:id/cancel` | JWT |
| POST | `/api/v1/jobs/:id/claim` | Runner Token |
| PATCH | `/api/v1/jobs/:id/status` | Runner Token |

List items include `team_id` (from the owning pipeline) so the UI team switcher can filter Jobs client-side. Visibility still follows membership / platform-admin rules via `team_in_clause`.

## Logs

| Method | Path | Auth |
|--------|------|------|
| POST | `/api/v1/jobs/:id/logs` | Runner Token |
| GET | `/api/v1/jobs/:id/logs` | JWT |

## Hosts

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/hosts` | JWT |
| POST | `/api/v1/hosts` | JWT |
| POST | `/api/v1/hosts/test` | JWT |
| PUT | `/api/v1/hosts/:id` | JWT |
| DELETE | `/api/v1/hosts/:id` | JWT |

### POST /hosts/test

Probe SSH connectivity using the current form values. Runs **from the Manager host** (not a Runner): connect + authenticate + `true`. Host keys are accepted for the probe only.

Request:
```json
{
  "host": "10.0.0.1",
  "port": 22,
  "username": "root",
  "credential_type": "key_path",
  "credential_data": "-----BEGIN OPENSSH PRIVATE KEY-----...",
  "credential_passphrase": "",
  "host_id": "optional-existing-host-uuid"
}
```

When `credential_data` is empty and `host_id` is set, Manager decrypts the stored credential. `credential_type: agent` cannot be probed from Manager (agent lives on the Runner).

Response:
```json
{ "code": 0, "data": { "ok": true, "latency_ms": 120, "message": "SSH OK — …" } }
```

`credential_type` values:

| Value | Meaning |
|-------|---------|
| `key_path` | Private key pasted in the Host form (encrypted in DB; sent to the runner at job dispatch) |
| `vault` | Password pasted in the Host form (encrypted in DB) |
| `agent` | No credential stored — the **runner** authenticates via its local SSH agent (`SSH_AUTH_SOCK` / Pageant). Keys must be loaded on the runner host (`ssh-add`) |

## Secrets

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/secrets` | JWT |
| POST | `/api/v1/secrets` | JWT |
| PUT | `/api/v1/secrets/:id` | JWT |
| DELETE | `/api/v1/secrets/:id` | JWT |

- Values are **AES-256-GCM encrypted at rest** (key derived from Manager `jwt_secret`). Plaintext rows are rejected at decrypt (re-save required).
- List/get return a **partially redacted** preview (`ab****yz`, or JSON with `password`/`token` fields masked) — never full plaintext.
- When ciphertext cannot be decrypted: `value` is `****` and `reencrypt_required: true`.
- Empty `value` on PUT keeps the existing ciphertext.
- `team_shared` (boolean, default `false` on create): when `true`, same-team pipelines auto-receive the secret at job claim; when `false`, use Authorized Secrets / YAML `secrets:` / step `secrets:` / `registry_secret`. See [Secrets Access Control](/docs/01-architecture/secrets-access-control/).
- For private registries, store JSON credentials and reference the secret **name** via step field `registry_secret` (see [Docker Engine](/docs/01-architecture/docker-engine/)). Prefer `team_shared: false` for registry JSON.

## Variables

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/variables` | JWT |
| POST | `/api/v1/variables` | JWT |
| PUT | `/api/v1/variables/:id` | JWT |
| DELETE | `/api/v1/variables/:id` | JWT |

## Webhooks

| Method | Path | Auth |
|--------|------|------|
| POST | `/api/v1/hooks/:path` | None (public) |

### POST /hooks/:path — External Webhook Trigger

Triggers the pipeline whose `webhook_path` matches `:path`.  Supports two
input styles:

**Query params**:

```
POST /api/v1/hooks/deploy-prod?version=1.2&env=staging
```

**JSON body** (flat keys, CI-style):

```json
{ "version": "1.2", "branch": "main" }
```

**JSON body** (`inputs` object — overrides query params):

```json
{ "inputs": { "version": "2.0" } }
```

Merge priority (highest wins): JSON `inputs` > flat JSON keys > query params.

Response:
```json
{ "code": 0, "data": { "job_id": "uuid", "pipeline_id": "uuid", "pipeline_name": "Deploy", "status": "pending" } }
```

Errors:
- `2001 NotFound` — no webhook pipeline registered for that path
- `2002 Conflict` — pipeline is deactivated or concurrency group is occupied

## Cron Scheduler

The Manager automatically scans pipelines with `trigger_type = "cron"` every
30 seconds. When a cron expression matches the current time, a new job is
created (triggered by `"cron"`). A minimum gap of 120 seconds between
trigger attempts prevents accidental double-fires.

Cron expressions follow the standard 5-field format (`min hour dom mon dow`)
and are automatically normalized to the 7-field format expected internally.
