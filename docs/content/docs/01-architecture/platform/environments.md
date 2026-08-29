---
title: Environments
weight: 7
description: Platform Environments overlay Variable and Secret defaults at Trigger / claim time
---

# Environments

## Mental model

```
platform_vars.value / secrets.value   ← shared Default (always)
    └─ environment override           ← optional, per Environment slug
         └─ pipeline env:             ← YAML still wins (vars path)
```

An **Environment** is a named platform scope (`staging`, `production`, …). It does
**not** replace Variables or Secrets — it only overrides a subset of their values
when a job is triggered with that Environment selected.

| Concept | Variables | Secrets |
|---------|-----------|---------|
| Default | `platform_vars.value` | `secrets.value` (AES) |
| Environment override | `environment_var_overrides` (plaintext) | `environment_secret_overrides` (AES) |
| GitHub per-repo override | `variable_github_binds.override_value` | `secret_github_binds.override_value` |
| Consumer | Buz job claim | Buz job claim / GitHub push |

These axes are **orthogonal**. Do not store Environment values on GitHub binds.

## Data model

```sql
CREATE TABLE environments (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    slug TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE environment_var_overrides (
    environment_id TEXT NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    var_id TEXT NOT NULL REFERENCES platform_vars(id) ON DELETE CASCADE,
    value TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (environment_id, var_id)
);

CREATE TABLE environment_secret_overrides (
    environment_id TEXT NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    secret_id TEXT NOT NULL REFERENCES secrets(id) ON DELETE CASCADE,
    value TEXT NOT NULL,   -- AES ciphertext (same scheme as secrets.value)
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (environment_id, secret_id)
);

-- jobs.environment_id TEXT NULL REFERENCES environments(id) ON DELETE SET NULL
```

## Claim merge order

**Variables**

1. Bound `platform_vars` (pipeline `variables:`)
2. Environment Variable overrides (same keys only — no inject)
3. Pipeline inline `env:` (YAML)
4. Trigger inputs

**Secrets**

1. Explicit `pipeline_secrets` + `team_shared` allowlist (and later implied
   `${secrets.X}` / purpose slots)
2. Environment Secret overrides for names **already on the allowlist**
3. Expansion / masking use the effective value

Unbound Environment overrides (Variable or Secret) are never injected.

## Trigger

`POST /pipelines/{id}/trigger` body may include:

```json
{ "inputs": {}, "environment": "production", "verbose": false }
```

- Omit / empty `environment` → legacy behavior (defaults only)
- Slug or id accepted; unknown → `404`

Job `snapshot.environment` records the slug for Job Detail UI.

## Protection / approval gates

When `protection_enabled` is true, Trigger creates `waiting_approval` jobs that
must be approved before concurrency / claim. **Full design** (principles, state
machine, append-only `job_approvals` records, API, deferrals):

→ **[Environment Approval Gates](./environment-approvals/)**

Summary defaults: `min_trigger_role=developer`, `min_approve_role=maintainer`,
`require_approvals=1`, `forbid_self_approve=true`, timeout `86400s`. Automated
triggers are rejected. Every approve / reject / cancel / timeout writes an
append-only row in `job_approvals` (exposed on `GET /jobs/{id}` as `approvals`).

## UI

- Infrastructure → **Environments**: CRUD (platform admin); Variable / Secret
  overrides; Protection card
- Trigger dialog: Environment dropdown; protected envs labeled
- Job Detail: Approve / Reject + **Approval history** trail

## API

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/environments` | Auth (Trigger dropdown) |
| POST | `/api/v1/environments` | PlatformAdmin (optional protection fields) |
| GET/PUT/DELETE | `/api/v1/environments/{id}` | GET Auth; mutate PlatformAdmin |
| GET | `/api/v1/environments/{id}/overrides` | Auth |
| PUT | `/api/v1/environments/{id}/overrides` | PlatformAdmin `{var_id, value}` |
| DELETE | `/api/v1/environments/{id}/overrides/{var_id}` | PlatformAdmin |
| GET | `/api/v1/environments/{id}/secret-overrides` | Auth (`has_override` only) |
| PUT | `/api/v1/environments/{id}/secret-overrides` | PlatformAdmin `{secret_id, value}` |
| DELETE | `/api/v1/environments/{id}/secret-overrides/{secret_id}` | PlatformAdmin |
| POST | `/api/v1/jobs/{id}/approve` | Auth ≥ `min_approve_role` (`{ comment? }`) |
| POST | `/api/v1/jobs/{id}/reject` | Auth ≥ `min_approve_role` (`{ comment? }`) |

Secret override list / upsert responses never include plaintext (`has_override: true` only).
See also [Environment Approval Gates](./environment-approvals/) and [API Reference](./api-reference/).

## Out of scope

- Per-environment Hosts
- YAML `environments:` allowlist on a pipeline
- Syncing Environment values to GitHub
- Approval IM notify (see deferrals in [Approval Gates](./environment-approvals/))
