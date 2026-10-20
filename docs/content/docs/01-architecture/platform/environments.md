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

An **Environment** is a named platform scope (`staging`, `production`, …). Pipeline YAML declares which registered slugs it may target with a required top-level `environments: [staging, production]` list; each trigger must select one of those slugs. The selected Environment does
**not** replace Variables or Secrets — it only overrides a subset of their values
when a job is triggered with that Environment selected.

| Concept | Variables | Secrets |
|---------|-----------|---------|
| Default | `platform_vars.value` | `secrets.value` (AES) |
| Environment override | `environment_var_overrides` (plaintext) | `environment_secret_overrides` (AES) |
| GitHub per-repo override | `variable_github_binds.override_value` | `secret_github_binds.override_value` |
| Consumer | Buz job claim | Buz job claim / GitHub push |

These axes are **orthogonal**. Do not store Environment values on GitHub binds.

## The `default` environment

A slug-`default` Environment ships with every deployment. It is seeded by an
idempotent migration on Manager startup ("auto-created home for pipelines
without an explicit environment", mirroring the `default` team) and is born
**unprotected**: `protection_enabled=false`, `deployment_concurrency=off` —
triggering it behaves like the pre-environment product. Pipelines must still
declare `default` in their top-level `environments:` list to target it;
auto-creation does not auto-bind.

**Deleting it** (Platform admin; deletion has no in-use guard) fails every
trigger path that targets it — a job can never run against a deleted
Environment:

| Path | What the user sees |
|------|--------------------|
| Dashboard trigger dialog | Empty environment picker ("no declared/registered environment"), Run disabled |
| Trigger of a file-backed (YAML) pipeline | `422` — `environment 'default' does not exist — create it under Environments first` |
| Trigger of other pipelines | `404` — `environment 'default' not found` |
| Cron / webhook / git triggers | No job is created; cron failures surface only in Manager logs |

**Recovery**: create a new environment with slug `default` (triggers resolve by
slug, not by the old id), or simply restart the Manager — the migration
re-seeds it automatically.

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

- Omit / empty `environment` → rejected (`422`); every trigger must include one declared slug
- Slugs are checked against both the pipeline allowlist and registered environments
- An `on.manual.inputs.environment` value is an ordinary parameter, not the selected deployment environment

Job `snapshot.environment` records the slug for Job Detail UI.

## Deployment concurrency

Each Environment has a Web-managed `deployment_concurrency` policy:

- `off` (default): no cross-pipeline environment gate; same-pipeline/environment jobs retain their normal serialization.
- `queue`: queue jobs targeting the same Environment across pipelines.
- `cancel_in_progress`: allow the newer deployment to replace an in-progress deployment targeting the same Environment.

The Manager evaluates and claims environment-gated jobs in a SQLite write transaction so separate pollers cannot claim the same environment concurrently.

## Host name / label allowlist

The Web environment detail manages Host names and Host labels; Hosts themselves remain global and have no Environment column. An empty allowlist means unrestricted. Once any entry exists, each SSH target must match a listed Host name or one of its labels. Dynamic Host names/labels are rejected for restricted Environments because they cannot be authorized before execution.

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

- Infrastructure → **Environments** (`/environments`): list + create, protection, deployment concurrency summary, and Host allowlist count
- Environment detail (`/environments/{id}`): inline edit for protection and deployment concurrency; Host name / label allowlist; Variable / Secret overrides below in a left/right split, each with its own pagination. List and detail use separate routes.
- Trigger dialogs show only the pipeline's declared registered environments and require a selection; protected envs are labeled
- Job Detail: Approve / Reject + **Approval history** trail

## API

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/environments` | Auth (Trigger dropdown) |
| POST | `/api/v1/environments` | PlatformAdmin (optional protection fields) |
| GET/PUT/DELETE | `/api/v1/environments/{id}` | GET Auth; mutate PlatformAdmin |
| GET | `/api/v1/environments/{id}/host-allowlist` | Auth |
| POST | `/api/v1/environments/{id}/host-allowlist` | PlatformAdmin `{kind: name|label, value}` |
| DELETE | `/api/v1/environments/{id}/host-allowlist/{kind}/{value}` | PlatformAdmin |
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

- Assigning Hosts directly to Environments (Hosts remain global; Environment-owned name/label allowlists provide targeting policy)
- Syncing Environment values to GitHub
- Approval IM notify (see deferrals in [Approval Gates](./environment-approvals/))
