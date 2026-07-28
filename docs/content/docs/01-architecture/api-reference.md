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
| POST | `/api/v1/auth/login` | None |
| GET | `/api/v1/auth/me` | JWT |
| GET | `/api/v1/auth/verify` | JWT |
| POST | `/api/v1/auth/change-password` | JWT |

### POST /auth/login

Request:
```json
{ "username": "admin", "password": "admin" }
```

Response:
```json
{ "code": 0, "data": { "token": "eyJ..." } }
```

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
{ "status": "ok", "name": "Buz", "version": "0.1.0", "component": "manager" }
```

Workspace version is defined in root `Cargo.toml` (`[workspace.package].version`) and mirrored in `frontend/package.json`. CLI: `buz-manager --version` / `buz-runner --version`.

## Runners

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/runners` | Admin JWT |
| GET | `/api/v1/runners/:id` | Admin JWT |
| POST | `/api/v1/runners` | Admin JWT |
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
  "version": "0.1.0"
}
```

Response:
```json
{
  "code": 0,
  "data": {
    "id": "uuid",
    "name": "prod-runner-01",
    "register_token": "one-time-token"
  }
}
```

### POST /runners/register — Runner Registration

Request:
```json
{
  "name": "prod-runner-01",
  "register_token": "one-time-token",
  "labels": ["linux-x64"],
  "version": "0.1.0"
}
```

Response:
```json
{
  "code": 0,
  "data": {
    "runner_id": "uuid",
    "token": "jwt"
  }
}
```

### POST /runners/heartbeat — Runner Heartbeat

Request (Bearer token):
```json
{ "status": "online" }
```

### GET /runners/poll — Runner Task Poll

Response (with task):
```json
{
  "code": 0,
  "data": {
    "job_id": "uuid",
    "pipeline_id": "uuid",
    "tasks": [
      { "id": "uuid", "step_order": 0, "name": "build", "exec_type": "local", "command_template": "npm run build", "work_dir": "" }
    ]
  }
}
```

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
| PUT | `/api/v1/hosts/:id` | JWT |
| DELETE | `/api/v1/hosts/:id` | JWT |

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

Secret values are masked (`***`) in list and get responses.

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
