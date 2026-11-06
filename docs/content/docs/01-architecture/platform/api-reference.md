---
title: API Reference
weight: 4
description: "Buz Manager REST API reference"
aliases:
  - /docs/01-architecture/api-reference/
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
| GET | `/health` | None |

### GET /health

Liveness probe (no auth). Returns version plus runtime health signals:
uptime, database connectivity with latency and pool usage, non-terminal job
backlog, WebSocket broadcast sink counts, and the step review-gate posture
(booleans only — the manifest path is not exposed).

```json
{
  "status": "ok",
  "name": "Buz",
  "version": "3.0.0",
  "component": "manager",
  "uptime_seconds": 3600,
  "started_at": "2026-08-30T12:00:00Z",
  "plugin_review": { "manifest_configured": false, "require_reviewed": false },
  "database": { "status": "ok", "latency_ms": 1.2, "pool": { "max": 8, "idle": 6, "in_use": 2 } },
  "jobs": { "pending": 0, "running": 1, "queued": 0, "waiting_approval": 0 },
  "ws": { "job_sinks": 1, "pipeline_sinks": 0 }
}
```

Semantics: default is **liveness** — HTTP 200 while the process responds. Pass
`?readiness=true` for a **readiness** probe that returns HTTP 503 when the
database is unavailable (`status` becomes `"degraded"`).

Workspace version is defined in root `Cargo.toml` (`[workspace.package].version`) and mirrored in `dashboard/package.json`. CLI: `buz-manager --version` / `buz-runner --version`.

## Runners

> Policy fields (`max_concurrent_jobs`, engines, cleanup, `ssh_host_key_policy`) are UI-managed. See [Runner Policy](/docs/01-architecture/execution/runner-concurrent-jobs/).

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/runners` | Admin JWT |
| GET | `/api/v1/runners/:id` | Admin JWT |
| POST | `/api/v1/runners` | Admin JWT |
| PUT | `/api/v1/runners/:id` | Admin JWT |
| DELETE | `/api/v1/runners/:id` | Admin JWT |
| PATCH | `/api/v1/runners/:id/freeze` | Admin JWT |
| POST | `/api/v1/runners/:id/restart` | Admin JWT |
| POST | `/api/v1/runners/:id/register-token/reissue` | Platform Admin JWT |
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
  "team_id": "uuid-or-null",
  "version": "1.3.0",
  "max_concurrent_jobs": 3,
  "clean_workspace_after_job": true,
  "shell_enabled": true,
  "ssh_enabled": true,
  "docker_enabled": true,
  "wasm_enabled": false,
  "ssh_host_key_policy": "insecure"
}
```

Policy fields are optional. Defaults: `max_concurrent_jobs=1`, cleanup off, shell+ssh on, docker+wasm off, `ssh_host_key_policy=known_hosts`.

`team_id` sets the dispatch scope: an existing team id binds the Runner to that team's jobs only; `null` creates an explicitly **Shared** Runner that accepts every team. Omitting the field also yields Shared (UI always sends an explicit choice). The team must exist; deleting a team with bound runners returns `409`.

Response:
```json
{
  "code": 0,
  "data": {
    "id": "uuid",
    "name": "prod-runner-01",
    "team_id": "uuid-or-null",
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
  "team_id": "uuid-or-null",
  "max_concurrent_jobs": 4,
  "docker_enabled": true,
  "ssh_host_key_policy": "known_hosts"
}
```

`team_id` is presence-aware: omit it to keep the current binding, send a team id to bind, or send `null` to make the Runner Shared. Changing the binding while jobs are running on the Runner returns `409`.

Returns the updated runner object (includes `running_jobs`, `team_id` and all policy fields). `register_token` is not included in the response.

List/get responses include policy columns (`max_concurrent_jobs`, `clean_workspace_after_job`, engine flags, `ssh_host_key_policy`), `team_id` and `running_jobs`; runner detail and update responses never expose `register_token`. The one-time token is returned only by runner creation and the explicit reissue endpoint.

### POST /runners/register — Runner Registration

Request:
```json
{
  "name": "prod-runner-01",
  "register_token": "one-time-token",
  "labels": ["linux-x64"],
  "version": "1.3.0"
}
```

The Runner stores its Manager-issued JWT in the durable local `token_cache_path` (default `./state/runner-token`), separate from its job workspace. Unix files are restricted to mode `0600`. A missing new cache may migrate the old `<workspace_dir>/runner-token` after successful registration. If the Manager reissues a token, the operator must still update the Runner's TOML or `BUZ_REGISTER_TOKEN` and restart it; online delivery and acknowledgement are not provided.

- `Authorization: Bearer <previous runner JWT>` — the agent persists its JWT and presents it on restart; expiry is ignored (refresh semantics), or
- a matching **admin-reissued** `register_token` (recovery path after the cached JWT is lost or `jwt_secret` was rotated; use the reissue API below, then update the Runner configuration).

Standalone mode keeps its loopback trust boundary for authorized names.

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

### POST /runners/:id/register-token/reissue — Reissue Runner Register Token

Requires a platform-admin bearer credential (user JWT or API token). This replaces the current one-shot token and returns the new value once; the token is never included in server logs or event/audit records.

```http
POST /api/v1/runners/<runner-id>/register-token/reissue
Authorization: Bearer <platform-admin-credential>
```

Response:
```json
{
  "code": 0,
  "data": { "register_token": "one-time-token" }
}
```

Immediately copy the token into the Runner's effective `register_token` setting (TOML or `BUZ_REGISTER_TOKEN`) and restart that Runner. The token is consumed on successful registration; the Runner then persists its JWT under `token_cache_path` (default `./state/runner-token`) and uses it for subsequent refreshes. Reissuing invalidates any previously stored register token. This endpoint does not update Runner configuration or restart it.

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
        "step_secrets": {}
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

Empty response: `204 No Content`

## Pipelines

Primary authoring is **YAML file APIs** under Manager `yaml_scan_dir` — see [YAML-First Authoring](/docs/01-architecture/authoring/yaml-first-authoring/). Legacy JSON body create/update remains only for old `source=form` rows.

| Method | Path | Auth | Notes |
|--------|------|------|-------|
| GET | `/api/v1/pipelines` | JWT | List + rate summary |
| POST | `/api/v1/pipelines/validate` | JWT | Syntax + schema + DB referential checks (viewer-scoped) |
| GET | `/api/v1/pipelines/yaml-files` | JWT | List files in `yaml_scan_dir` (+ `scan_dir`) |
| POST | `/api/v1/pipelines/yaml-files` | JWT | Create file + upsert projection |
| GET | `/api/v1/pipelines/yaml-files/:filename` | JWT | Read file; returns `etag` |
| PUT | `/api/v1/pipelines/yaml-files/:filename` | JWT | Overwrite + upsert; body `etag` or `If-Match` → 409 on mismatch |
| DELETE | `/api/v1/pipelines/yaml-files/:filename` | JWT | Delete file + pipeline row |
| GET | `/api/v1/pipelines/:id` | JWT | Includes `yaml_sync` for file-backed pipelines |
| GET | `/api/v1/pipelines/:id/yaml-source` | JWT | Authority YAML for `source=yaml`; returns `etag` |
| POST | `/api/v1/pipelines` | JWT | **Legacy** JSON create (`source=form`) |
| PUT | `/api/v1/pipelines/:id` | JWT | **Legacy** JSON update; **409** when `source=yaml` |
| DELETE | `/api/v1/pipelines/:id` | JWT | |
| PATCH | `/api/v1/pipelines/:id/toggle-active` | JWT | Deactivate sticks across YAML rescans |
| POST | `/api/v1/pipelines/:id/disable` | JWT | Idempotent emergency stop; body `{ "cancel_in_flight": true }` cancels all non-terminal jobs in the same transaction |
| POST | `/api/v1/pipelines/:id/enable` | JWT | Idempotent re-enable; jobs cancelled by disable stay cancelled |
| POST | `/api/v1/pipelines/:id/trigger` | JWT | Re-reads/validates file first when `source=yaml`; `verbose` / `debug` (debug = platform admin only); **409** when pipeline disabled |

### GET /pipelines — List

Each item includes latest-job summary and **build success rate**:

| Field | Type | Description |
|-------|------|-------------|
| `job_total` | number | Total jobs (builds) for this pipeline |
| `job_success` | number | Jobs with `status = success` |
| `success_rate` | number \| null | `job_success / job_total` (0–1). `null` when `job_total` is 0 |

UI column: **Rate** (percentage + `success/total` hint).

### GET /pipelines/:id — Detail

For `source=yaml`, response includes `yaml_sync`:

| `yaml_sync.status` | Meaning |
|--------------------|---------|
| `in_sync` | File mtime matches last projection |
| `stale` | File newer than last sync (Trigger refreshes) |
| `validation_failed` | On-disk YAML invalid |
| `missing_file` | Basename missing under `yaml_scan_dir` |
| `not_file_backed` | `source≠yaml` |

### POST /pipelines/yaml-files — Create (preferred)

Request:
```json
{
  "yaml": "name: hello-world\nsteps:\n  - name: hi\n    run: echo hello\n",
  "filename": "hello-world.yml"
}
```

`filename` optional → slug from YAML `name:`. Writes under flat `yaml_scan_dir` (basename only).

### POST /pipelines/:id/trigger — Trigger Run

Body requires `environment` and may include `inputs`, `verbose`, and `debug` (`debug` is rejected for non–platform-admin). The selected registered environment slug must be declared by the pipeline; missing, unknown, or undeclared environments are rejected. A declared slug whose Environment was deleted also fails (`422` for file-backed pipelines, `404 environment '<slug>' not found` otherwise); see [Environments](./environments/) for the `default` environment lifecycle.

Response:
```json
{
  "code": 0,
  "data": { "job_id": "uuid" }
}
```

Job `snapshot.environment` records the selected slug when set.

## Jobs

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/jobs` | JWT |
| GET | `/api/v1/jobs/:id` | JWT |
| POST | `/api/v1/jobs/:id/cancel` | JWT (`pending` / `running` / `queued` / `waiting_approval`) |
| POST | `/api/v1/jobs/:id/approve` | JWT ≥ env `min_approve_role` (optional `{ comment }`) |
| POST | `/api/v1/jobs/:id/reject` | JWT ≥ env `min_approve_role` (optional `{ comment }`) |
| POST | `/api/v1/jobs/:id/claim` | Runner Token |
| PATCH | `/api/v1/jobs/:id/status` | Runner Token |

List/get include `trigger_user_id` when set. `GET /jobs/:id` returns append-only `approvals[]` (`approve` \| `reject` \| `cancel` \| `timeout`). Protected Environment triggers create `waiting_approval` jobs (`approval_required: true` on trigger response) until approve — see [Environment Approval Gates](./environment-approvals/). Concurrent approve/reject/cancel on one job are serialized per job: exactly one decision wins and each loser gets `409 Conflict` with no side effects (race semantics in [Environment Approval Gates](./environment-approvals/)).

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
- `team_shared` (boolean, default `false` on create): when `true`, same-team pipelines auto-receive the secret at job claim; when `false`, use Authorized Secrets / YAML `secrets:` / step `secrets:`. See [Secrets Access Control](/docs/01-architecture/platform/secrets-access-control/).
- `kind` (`generic` \| `registry`, default `generic`): `registry` is a legacy value retained on existing rows — no longer creatable or validated (kernel support removed in v3). List/get return `kind`.
- For private registries, use the `buz/docker-login` plugin template with plain `generic` secrets (see [Docker Engine](/docs/01-architecture/execution/docker-engine/)). Prefer `team_shared: false` for registry credentials.

## Variables

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/variables` | JWT |
| POST | `/api/v1/variables` | JWT |
| PUT | `/api/v1/variables/:id` | JWT |
| DELETE | `/api/v1/variables/:id` | JWT |

Shared **Default** lives in `platform_vars.value`. Per-run overlays: [Environments](./environments/) (Variables + Secrets). Per-repo GitHub push overlays: bind `override_value` (see Sync below).

## Environments

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/environments` | JWT |
| POST | `/api/v1/environments` | PlatformAdmin (optional protection fields) |
| GET | `/api/v1/environments/:id` | JWT |
| PUT | `/api/v1/environments/:id` | PlatformAdmin (optional protection fields) |
| DELETE | `/api/v1/environments/:id` | PlatformAdmin |
| GET | `/api/v1/environments/:id/overrides` | JWT |
| PUT | `/api/v1/environments/:id/overrides` | PlatformAdmin `{var_id, value}` |
| DELETE | `/api/v1/environments/:id/overrides/:var_id` | PlatformAdmin |
| GET | `/api/v1/environments/:id/secret-overrides` | JWT (`has_override` only — never plaintext) |
| PUT | `/api/v1/environments/:id/secret-overrides` | PlatformAdmin `{secret_id, value}` |
| DELETE | `/api/v1/environments/:id/secret-overrides/:secret_id` | PlatformAdmin |

Protection fields: `protection_enabled`, `min_trigger_role`, `min_approve_role`, `require_approvals`, `forbid_self_approve`, `approval_timeout_seconds`. Full merge order: [Environments](./environments/). Approval gates + records: [Environment Approval Gates](./environment-approvals/).

## GitHub Actions Sync

See [GitHub Actions Secrets/Variables Sync](/docs/01-architecture/platform/github-actions-sync/).

**Connections / repos / repo fetch / test** — `PlatformAdmin`:

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/github-sync/connections` | PlatformAdmin |
| POST | `/api/v1/github-sync/connections` | PlatformAdmin |
| PUT | `/api/v1/github-sync/connections/:id` | PlatformAdmin |
| DELETE | `/api/v1/github-sync/connections/:id` | PlatformAdmin |
| GET | `/api/v1/github-sync/repos` | PlatformAdmin |
| POST | `/api/v1/github-sync/repos` | PlatformAdmin |
| PUT | `/api/v1/github-sync/repos/:id` | PlatformAdmin |
| DELETE | `/api/v1/github-sync/repos/:id` | PlatformAdmin |
| POST | `/api/v1/github-sync/repos/fetch` | PlatformAdmin |
| POST | `/api/v1/github-sync/test` | PlatformAdmin |

Connection create/update accept optional auto-fetch fields
`fetch_owners` (comma/newline-separated owners) and `fetch_interval_minutes`
(positive integer, or `null` to disable). On update, absent fetch fields keep
their current value; explicit `null` clears owners / disables the interval.
`GET /connections` returns `fetch_owners`, `fetch_interval_minutes`, and
`last_fetch_at` alongside the existing fields. Auto-fetch runs on a leader-
leased scheduler (60s tick, shared `scheduler_lease` row `id=2`); manual and
scheduled fetches both update `last_fetch_at`.

`GET /api/v1/github-sync/repos` returns each repo with a `binds` array listing
the secrets / variables bound to it: `{kind: "secret"|"variable", id, name,
last_status, last_op}` — metadata only, never values.

`POST /api/v1/github-sync/test` verifies a connection token. When `owner`
(and optionally `repo`) is provided it checks read access to that owner / repo;
when both are omitted it validates the token alone via `GET /user` and returns
`{ok: true, login: "<account>"}`.

**Secret binds** — `GET` is `TeamView` (read-only status); mutations are
`PlatformAdmin` (they push using a connection PAT):

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/secrets/:id/github-binds` | TeamView |
| POST | `/api/v1/secrets/:id/github-binds` | PlatformAdmin |
| PATCH | `/api/v1/secrets/:id/github-binds/:bindId` | PlatformAdmin |
| DELETE | `/api/v1/secrets/:id/github-binds/:bindId` | PlatformAdmin |
| POST | `/api/v1/secrets/:id/github-binds/push` | PlatformAdmin |

**Variable binds** — `PlatformAdmin`:

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/variables/:id/github-binds` | PlatformAdmin |
| POST | `/api/v1/variables/:id/github-binds` | PlatformAdmin |
| PATCH | `/api/v1/variables/:id/github-binds/:bindId` | PlatformAdmin |
| DELETE | `/api/v1/variables/:id/github-binds/:bindId` | PlatformAdmin |
| POST | `/api/v1/variables/:id/github-binds/push` | PlatformAdmin |

- Variable / secret bind body may include optional `override_value` (omit/`null` =
  inherit platform default; string = per-repo Actions value). `PATCH` sets
  `{ "override_value": string | null }` (`null` clears to inherit). Secret
  overrides are encrypted at rest; list responses expose `has_override` only
  (never plaintext). Push resolves `effective = override ?? default` per bind —
  no value in the push request body.
- Secret pushes seal the plaintext with libsodium `crypto_box_seal` (fresh key
  per push) and `PUT` to GitHub Actions; variable pushes `POST`/`PATCH` the
  value. Per-repo results are returned and persisted to the bind row.
- Unbinding (`DELETE …/github-binds/:bindId`) is **remote-first**: it deletes
  the value from GitHub Actions and only then removes the local bind row
  (idempotent — never-pushed binds still complete; a failed remote cleanup
  keeps the bind and returns the error).
- `GET /api/v1/secrets` and `GET /api/v1/variables` each gain a `github_binds`
  array per row (bind metadata only — never values).

## Webhooks

| Method | Path | Auth |
|--------|------|------|
| POST | `/api/v1/hooks/:path` | None (public) — generic HTTP trigger |
| POST | `/api/v1/hooks/git/:provider/:hook_key` | Platform Secret/Token (planned) — GitHub/Gitee |

Generic hooks use query / flat JSON / `inputs` (below). Git platform push events use a **separate** adapter with URL + independent Secret (not in the URL). Design: [Git Webhook Triggers](/docs/01-architecture/authoring/git-webhook-triggers/).

### POST /hooks/:path — External Webhook Trigger

Triggers the pipeline whose `webhook_path` matches `:path`.  Supports two
input styles:

**Deployment environment selector** (required for pipelines with declared environments): use `environment` in the query string, top-level JSON body, or JSON `inputs` object. Query takes precedence over body; body takes precedence over the pipeline's configured `on.webhook.environment` default. The selector must name an environment registered in Manager and declared by the pipeline. It is reserved and is not passed as a business input. If neither the request nor the YAML default supplies it, the trigger is rejected; `env` remains an ordinary input and is not an alias.

**Query params**:

```
POST /api/v1/hooks/deploy-prod?version=1.2&environment=staging
```

**JSON body** (flat keys, CI-style):

```json
{ "environment": "staging", "version": "1.2", "branch": "main" }
```

**JSON body** (`inputs` object — overrides query params for business inputs):

```json
{ "environment": "staging", "inputs": { "version": "2.0" } }
```

Merge priority for business inputs (highest wins): JSON `inputs` > flat JSON keys > query params. The environment selector is resolved separately using query > body > YAML default.

Response:
```json
{ "code": 0, "data": { "job_id": "uuid", "pipeline_id": "uuid", "pipeline_name": "Deploy", "status": "pending" } }
```

Errors:
- `2001 NotFound` — no webhook pipeline registered for that path
- `2002 Conflict` — pipeline is deactivated or concurrency group is occupied

### POST /hooks/git/:provider/:hook_key — Git platform adapter

- `{provider}`: `github` | `gitee`
- URL has **no** secret query param; verify via GitHub `X-Hub-Signature-256` or Gitee `X-Gitee-Token` against the secret stored (encrypted) on `pipeline_git_hooks` — user pastes the same secret into the platform Webhook UI.
- On success, triggers a job with normalized `INPUT_GIT_*` (see [Git Webhook Triggers](/docs/01-architecture/authoring/git-webhook-triggers/)).
- Skipped deliveries return HTTP 202; bad signature → 401 (`AuthFailed`).

### Git hook config (JWT) — multiple per pipeline

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/v1/pipelines/:id/git-hooks` | `{ items: [...] }` public fields (no secret) |
| POST | `/api/v1/pipelines/:id/git-hooks` | Create with required `environment`; must be registered and declared by the pipeline. Returns plaintext `secret` once. Environment binding is immutable afterward. |
| DELETE | `/api/v1/pipelines/:id/git-hooks/:hook_id` | Remove one hook (recreate to change settings) |

Git Hook create body requires `provider` and `environment`, and may include `label`, `branch_filter`, and `enabled`. The selected environment is fixed for every delivery to that hook; delete and recreate the hook to change its target. Cron YAML likewise requires an explicit `on.cron.environment` declared in the top-level `environments` list.

## Audit logs

Platform-admin only. See [Admin Audit Logs](/docs/01-architecture/platform/admin-audit-logs/).

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/audit-logs` | JWT + PlatformAdmin |

Query: `page`, `page_size` (1–100), `action`, `actor` (username substring), `resource_type`, `q` (search), `from`, `to` (created_at bounds).

Response `data`: `{ items, total, page, page_size }`. Items include summary fields plus redacted `before` / `after` / `meta` JSON.

**Tiered visibility**: super admins see every record; regular platform admins only see records where they are the actor (`actor_user_id` matches their own id).

## API Tokens

Long-lived bearer credentials for scripts and tooling (`Authorization: Bearer <token>`), created by platform admins. Only the SHA-256 hash is stored; the plaintext is returned exactly once at creation.

**Tokens are operations-plane credentials.** Regardless of scope, every API token is denied:

- the **identity plane** — user management, token management (no self-perpetuation), password change;
- the **governance plane** — platform settings, plugin sources/pool curation.

Those endpoints require a browser session (user JWT). Within the operations plane (pipelines, jobs, runners, hosts, secrets, variables, teams, webhooks, audit logs, …), the token's scope bounds the HTTP methods:

| Scope | Operations plane | Requested via |
|-------|------------------|---------------|
| `read` (default) | Safe methods only (GET/HEAD/OPTIONS) | New tokens |
| `ops` | Any method | New tokens |
| `full` | Any method (creator-equivalent band) | **Legacy only** — pre-scope tokens keep it until revoked; closed to new tokens |

Out-of-scope requests fail with 403 and a message naming the denied plane or the read-only band.

| Method | Path | Auth |
|--------|------|------|
| POST | `/api/v1/api-tokens` | Session JWT + PlatformAdmin |
| GET | `/api/v1/api-tokens` | Session JWT + PlatformAdmin |
| DELETE | `/api/v1/api-tokens/:id` | Session JWT + PlatformAdmin |

**Tiered visibility**: super admins list and revoke every token platform-wide; regular platform admins only list/revoke their own.

### POST /api-tokens — Create

Request:
```json
{ "name": "ci-script", "scope": "ops" }
```

`name` must be 1–64 chars of `[A-Za-z0-9._-]`. `scope` is `"read"` (default) or `"ops"`; the legacy `"full"` value is rejected for new tokens. Response `data`:

```json
{
  "id": "...",
  "name": "ci-script",
  "token": "buz_a1b2c3…",
  "prefix": "buz_a1b2c3…",
  "created_at": "…",
  "scope": "ops"
}
```

The `token` field is only present in this response. Store it before closing the dialog — it cannot be retrieved again. Creates an `api_token.create` audit event (the scope is recorded in the event payload).

### GET /api-tokens — List

Response `data`: `{ items: [{ id, user_id, username, name, prefix, created_at, last_used_at, revoked, revoked_at, scope }] }`.

### DELETE /api-tokens/:id — Revoke

Immediately invalidates the token; subsequent requests with it return 401. Creates an `api_token.revoke` audit event. Regular admins may only revoke their own tokens.

## Cron Scheduler

The Manager automatically scans pipelines with `trigger_type = "cron"` every
30 seconds. When a cron expression matches the current time, a new job is
created (triggered by `"cron"`). A minimum gap of 120 seconds between
trigger attempts prevents accidental double-fires.

Cron expressions follow the standard 5-field format (`min hour dom mon dow`)
and are automatically normalized to the 7-field format expected internally.
