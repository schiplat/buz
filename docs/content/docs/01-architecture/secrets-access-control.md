---
title: Secrets Access Control
weight: 4
description: "How Buz controls which pipelines can access secrets, variables, and hosts — owner_group, explicit binding, and three-tier resolution"
---

## Overview

Buz implements a **lightweight namespace model** for secret access control. Every secret and pipeline can belong to an `owner_group`, and access is granted through a combination of **group-level auto-access** and **explicit per-pipeline bindings**.

Platform **Variables** and **Hosts** use a simpler binding model (explicit ID/name references) — see [Resource Binding Map](#resource-binding-map) below.

This is **Scheme B** — chosen over simpler alternatives (e.g. "any pipeline can use any secret") to provide meaningful isolation without requiring a full user/team management system in Phase 1.

---

## owner_group

`owner_group` is a plain-text label that acts as a **resource namespace**. It serves the same conceptual role as:

| Buz | Equivalent in other systems |
|-----|----------------------------|
| `owner_group` | Kubernetes namespace, GitHub organization, GitLab group |

### Where owner_group appears

| Resource | Field | Meaning |
|----------|-------|---------|
| Secret | `owner_group` | This secret belongs to this group |
| Pipeline | `owner_group` | This pipeline belongs to this group |

### Rule

**Secrets whose `owner_group` equals a pipeline's `owner_group` are automatically accessible to that pipeline at runtime.** No additional configuration needed.

```
Secret "DOCKER_PASS" owner_group = "prod-team"
Pipeline "Deploy API" owner_group = "prod-team"
→ Runner automatically receives DOCKER_PASS as an env var
```

If `owner_group` is empty on one or both sides, no auto-access is granted — only explicit bindings take effect.

### Current State

- **Phase 2 (current):** Pipelines and Secrets require a Team (`team_id`). UI uses a Team dropdown. Same-team secrets auto-inject at job dispatch. See [Users, Teams & RBAC](/docs/01-architecture/rbac/).
- `owner_group` remains as the Team **slug** string for display / YAML compat (`team:` / `owner_group:` in YAML).
- YAML scanner resolves `team:` to `team_id`, falling back to bootstrapped `default`

---

## Three-Tier Secret Resolution

When the runner polls for a job, the manager resolves secrets from **three sources** in order:

### Tier 1: Group-Level Auto-Access

Secrets whose `owner_group` matches the pipeline's `owner_group`.

```sql
-- Conceptual: manager matches pipeline.owner_group against secrets.owner_group
SELECT * FROM secrets WHERE owner_group = ? AND owner_group != ''
```

**Use case:** A team has many pipelines that all need the same Docker registry token. Put everything in `owner_group: "platform"` and all pipelines get it automatically.

### Tier 2: Explicit Per-Pipeline Binding

Secrets explicitly bound to this specific pipeline via the `pipeline_secrets` table.

```sql
SELECT s.* FROM secrets s
JOIN pipeline_secrets ps ON ps.secret_id = s.id
WHERE ps.pipeline_id = ?
```

**Use case:** Pipeline A needs a secret that belongs to a different group (or a group-agnostic secret). The admin explicitly authorizes it in the form's **Authorized Secrets** multi-select.

### Tier 3: Legacy YAML Name Reference

Secrets referenced by **name** (not ID) in the pipeline YAML's `secrets:` list or a step's `secrets:` list. At scan/import time these names are resolved to IDs and written as explicit bindings when possible; unresolved names warn in the UI.

**Use case:** YAML-defined pipelines under `pipelines/`. Prefer creating the secret in Infrastructure → Secrets first, then referencing it by name.

### Resolution Order

All tiers are evaluated, results are **merged and de-duplicated by name**, and the final set is sent to the runner in the poll response:

```json
{
  "task": { ... },
  "secrets": {
    "DOCKER_PASS": "hunter2",
    "GIT_SSH_KEY": "-----BEGIN OPENSSH PRIVATE KEY-----..."
  }
}
```

---

## Resource Binding Map

How each resource type connects to a pipeline:

| Resource | Where created | How bound to a pipeline | Runtime injection |
|----------|---------------|-------------------------|-------------------|
| **Secrets** | Infrastructure → Secrets | (1) Matching `owner_group`, or (2) Advanced → **Authorized Secrets** (`secret_ids` → `pipeline_secrets`), or (3) YAML `secrets: [NAME]` | Env vars `$NAME`, masked in logs |
| **Registry credentials** | Same Secrets store (JSON value) | Step field `registry_secret: <name>` (form: Registry Secret) | `docker login` only — **not** injected as `$NAME` (except transient `BUZ_REGISTRY_*` on SSH) |
| **Variables** | Infrastructure → Variables | Advanced → **Variables** (`var_ids` → `pipeline_variables`), or YAML `variables: [NAME]` | Env vars `$NAME` (not masked) |
| **Hosts** | Infrastructure → Hosts | Step fields `host` / `host_labels` (name or label match) when `uses: ssh` | SSH connection params on the runner — not env vars. Auth may be key/password (from Manager) or `agent` (runner's `SSH_AUTH_SOCK`) |
| **Inline Env** | Pipeline form Env editor / YAML `env:` | Always scoped to that pipeline | Env vars for every step |

---

## Pipeline Secrets Table

The `pipeline_secrets` table implements **many-to-many explicit bindings**:

| Column | Type | Notes |
|--------|------|-------|
| pipeline_id | TEXT FK → pipelines.id | ON DELETE CASCADE |
| secret_id | TEXT FK → secrets.id | ON DELETE CASCADE |
| UNIQUE(pipeline_id, secret_id) | | Prevents duplicate bindings |

Bindings are created when:

1. **Form**: A user selects secrets in Advanced → **Authorized Secrets**
2. **YAML Scan / Import**: Names in `secrets:` are resolved to IDs when they exist in Manager
3. **API**: `POST` / `PUT` pipelines with `secret_ids` in the payload

Variables use an analogous `pipeline_variables` table via `var_ids`.

---

## UI Fields

### New Pipeline / Edit Pipeline Form (Advanced)

| Field | Type | Purpose |
|-------|------|---------|
| **Owner Group / Team** | Team dropdown (required) | Sets `pipelines.team_id` (+ slug in `owner_group`). Same-team secrets auto-accessible. |
| **Variables** | Multi-select | Explicitly binds platform variables by ID (`var_ids`). Injected as `$NAME` in every step. |
| **Authorized Secrets** | Multi-select | Explicitly binds secrets by ID (`secret_ids` → `pipeline_secrets`). Labels show `NAME · team`. |
| **Env** | Key/value list | Pipeline-only inline env (not from Infrastructure). |

### New Secret / Edit Secret Form

| Field | Type | Purpose |
|-------|------|---------|
| **Name** | Text (required) | Secret name — used as env var key |
| **Owner Group / Team** | Team dropdown (required) | Sets `secrets.team_id`. Pipelines in the same team auto-receive this secret. |
| **Value** | Password input | **AES-256-GCM encrypted at rest** (key derived from Manager `jwt_secret`). List/get return a **partially redacted** preview (`ab****yz`, or JSON with `password`/`token` fields masked). Never full plaintext. Empty value on update keeps the existing ciphertext. Plaintext DB rows are rejected — set `reencrypt_required` and re-save. |

### Variables / Hosts pages

| Page | Binding reminder in UI |
|------|------------------------|
| **Variables** | Bind via Pipeline → Advanced → Variables (or YAML `variables:`) |
| **Hosts** | Reference in SSH steps with `host: name` or `host_labels: [tag]` |

---

## YAML ↔ Form Mapping

| YAML | Form | Behavior |
|------|------|----------|
| `secrets: [GIT_SSH_KEY, DOCKER_PASS]` | Authorized Secrets (after name→ID resolve) | Explicit `pipeline_secrets` binding; injected as env `$NAME` |
| `variables: [BUILD_CACHE_DIR]` | Variables multi-select | Explicit `pipeline_variables` binding |
| _(no YAML field)_ | Owner Group | Auto-access by group match |
| `steps[].host` / `host_labels` | Step → SSH Host / Host Labels | Resolves Infrastructure → Hosts at dispatch |
| `steps[].registry_secret` | Registry Secret | Secret **name** only; JSON value used for `docker login` on shell / SSH / docker — **not** injected as env |

---

## Registry credentials (`registry_secret`)

For private registries (e.g. Alibaba Cloud ACR Personal Edition), store JSON in Infrastructure → Secrets and reference the **name** on the step:

```yaml
registry_secret: acr-personal
```

```json
{"provider":"acr_personal","registry":"…","username":"…","password":"…"}
```

- Password is encrypted at rest; API returns partial redaction only.
- Docker: login before pull. Shell/SSH: login before `run` (for push). See [Docker Engine — ACR Personal Edition](/docs/01-architecture/docker-engine/).

---

## Best Practices

1. **Use owner_group for team-wide secrets** — docker registry tokens, CI tokens shared by all pipelines in a team
2. **Use Authorized Secrets for cross-group access** — a pipeline in `staging` that needs a secret owned by `prod`
3. **Use step-level secrets (YAML) for scoping** — a Git SSH key that only the `checkout` step needs
4. **Private registry login** — store ACR (etc.) JSON credentials in Infrastructure → Secrets; reference only the **name** via `registry_secret` on docker/shell/SSH steps (never put passwords in YAML)
5. **Keep owner_group values consistent** — decide on naming (`platform`, `backend-team`, `prod`) and apply them uniformly
6. **Create Variables/Secrets/Hosts in Infrastructure first**, then bind from the pipeline form or YAML

---

## Future: Phase 2+ RBAC

Full target design (confirmed): **[Users, Teams & RBAC](/docs/01-architecture/rbac/)**.

| Phase | Capability |
|-------|-----------|
| Phase 1 (current baseline) | `owner_group` as plain text, three-tier resolution, single admin login |
| Phase 2 (**implemented**) | `users` + `teams` + four team roles; pipelines/secrets **require** `team_id`; hosts optional; list filtering by membership — see [RBAC](/docs/01-architecture/rbac/) |
| Phase 3 | SSO, API tokens, audit UI, per-step secrets, cross-team grants |

The `owner_group` field is intentionally kept as a plain string in Phase 1 so the access control logic can be battle-tested before layering user management on top.
