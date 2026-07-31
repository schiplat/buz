---
title: Secrets Access Control
weight: 4
description: "How Buz controls which pipelines can access secrets — kind, team_shared opt-in, explicit binding, and three-tier resolution"
---

## Overview

Buz controls secret delivery with a **team ownership boundary** plus an **opt-in team share flag**. Every secret and pipeline belongs to a Team (`team_id`). Runtime injection is granted through:

1. **Team auto-access** — only when the secret has `team_shared = true` and shares the pipeline’s `team_id`
2. **Explicit per-pipeline bindings** — `pipeline_secrets` / YAML `secrets:`
3. **Explicit step references** — step `secrets:` or `registry_secret:`

**Default for new secrets: `team_shared = false`.** Same-team pipelines do **not** receive the secret unless they bind or reference it. Cross-team use remains forbidden (future Phase 3 grants).

This is **Scheme B** — chosen over “any pipeline can use any secret” to provide isolation without a full IAM product in early phases. The share flag corrects the earlier over-broad assumption that “same team ⇒ always inject.”

---

## `team_shared` (opt-in auto-inject)

| | |
|---|---|
| Column | `secrets.team_shared` (`INTEGER` 0/1, API `boolean`) |
| Default (create) | `false` |
| Migration (existing rows) | `false` (secure default; re-enable share or bind as needed) |
| Who may toggle | Team role with `SecretManage` (Maintainer+) |

### Semantics

| `team_shared` | Same-team pipeline behavior |
|---------------|-----------------------------|
| `false` (default) | **Not** auto-injected. Use Authorized Secrets, YAML `secrets:`, step `secrets:`, or `registry_secret`. |
| `true` | Auto-injected into every job for pipelines in the same team (legacy convenience for shared CI tokens). |

`team_shared` does **not** mean “public plaintext” or “cross-team readable.” It only means: *allow Tier‑1 auto-access within the owning team.*

### Recommended usage

| Secret type | Suggested setting | How pipelines use it |
|-------------|-------------------|----------------------|
| Shared CI token used by many pipelines as `$NAME` | `kind: generic`, `team_shared: true` | Auto env inject |
| Prod DB password, deploy key | `kind: generic`, `team_shared: false` | Explicit bind / step `secrets:` |
| Registry login JSON (ACR, etc.) | `kind: registry`, `team_shared: false` | `registry_secret:` only (not as `$NAME` env) |

---

## `kind` (`generic` | `registry`)

| | |
|---|---|
| Column | `secrets.kind` (`TEXT`, API string) |
| Values | `generic` (default) · `registry` |
| Who may set | Team role with `SecretManage` (Maintainer+) |

| `kind` | Meaning |
|--------|---------|
| `generic` | Env inject / Authorized Secrets / step `secrets:`. **Not** listed in Pipeline **Registry Secret** dropdown. |
| `registry` | JSON credentials (`username` + `password`, optional `registry` / `provider`). Required for `registry_secret` at claim; validated on create/update. |

Migration backfill: decryptable values that parse as registry JSON are set to `kind=registry`. Claim rejects `registry_secret` pointing at a non-registry secret.

---

## Team + `owner_group`

| Field | Role |
|-------|------|
| `team_id` | Ownership, list filtering, CRUD RBAC, runtime same-team checks |
| `owner_group` | Team **slug** string for display / YAML compat (`team:` / `owner_group:`) |

Phase 1 used free-form `owner_group` equality for auto-access. Phase 2 prefers `team_id`; auto-access additionally requires `team_shared`.

---

## Three-Tier Secret Resolution

When the runner polls for a job, the manager resolves secrets as follows:

### Tier 1: Team Auto-Access (opt-in)

```sql
SELECT s.name, s.value
FROM secrets s
JOIN pipelines p ON p.team_id = s.team_id AND p.team_id != ''
WHERE p.id = ? AND s.team_shared = 1
```

**Use case:** A team-wide Docker registry password every pipeline needs as `$DOCKER_REGISTRY_PASS` without per-pipeline binding.

### Tier 2: Explicit Per-Pipeline Binding

Secrets bound via `pipeline_secrets` (form **Authorized Secrets** or YAML top-level `secrets:`).

```sql
SELECT s.* FROM secrets s
JOIN pipeline_secrets ps ON ps.secret_id = s.id
WHERE ps.pipeline_id = ?
```

Bind-time auth: binder needs `SecretManage` on the pipeline’s team; secret must belong to the **same** team (cross-team forbidden). YAML scanner resolves `secrets:` names with `AND team_id = pipeline.team_id`. Job claim Tier 2 also requires `s.team_id = p.team_id` so stale cross-team rows never inject.

**Use case:** Pipeline needs a non-shared secret for all steps as env vars.

### Tier 3: Explicit name reference (step / registry)

| Reference | Injection |
|-----------|-----------|
| Step `secrets: [NAME]` | Env vars for **that step only** |
| Step `registry_secret: NAME` | `docker login` credentials only — **not** injected as `$NAME` |

Resolution requires the secret to exist and `secret.team_id == pipeline.team_id`. Name-only lookup across teams is rejected.

**Use case:** Git deploy key only on checkout; ACR JSON only for private pull/push.

### Resolution Order

Tiers are merged and de-duplicated by name. Explicit bindings / step maps win over auto-access when names collide.

Claim payload (Manager → Runner):

```json
{
  "job_id": "…",
  "pipeline_id": "…",
  "tasks": [ { "step_secrets": {}, "registry_auth": null } ],
  "inputs": { "tag": "1.0", "deploy_token": "…" },
  "masking_input_keys": ["deploy_token"],
  "env": {
    "SMOKE_ENV": "from-pipeline",
    "REGISTRY": "registry.example.com"
  },
  "secrets": {
    "DOCKER_PASS": "hunter2"
  }
}
```

`secrets`, registry passwords / step secrets, and keys listed in `masking_input_keys` are redacted in logs. Plaintext `inputs` are still injected as env for execution. See [Resource Binding Map](#resource-binding-map).
---

## Resource Binding Map

| Resource | Where created | How bound to a pipeline | Runtime injection |
|----------|---------------|-------------------------|-------------------|
| **Secrets** | Infrastructure → Secrets | (1) `team_shared` + same team, or (2) **Authorized Secrets** / YAML `secrets:`, or (3) step `secrets:` / `registry_secret` | Env `$NAME` (**masked in logs**), except `registry_secret` → login only |
| **Registry credentials** | Same Secrets store (JSON) | Step `registry_secret: <name>` | `docker login` only |
| **Variables** | Infrastructure → Variables | Advanced → Variables / YAML `variables:` | Env `$NAME` (**not masked**) |
| **Hosts** | Infrastructure → Hosts | Step `host` / `host_labels` when `uses: ssh` | SSH params on runner |
| **Inline Env** | Pipeline `env:` | Always scoped to that pipeline | Env for every step (**not masked**) |
| **Trigger inputs** | `on.manual.inputs` / trigger dialog | Per job | Env `$NAME` / `$INPUT_NAME`; **`masking: true` → masked in logs** |

At job claim the Manager sends:

- `secrets` — Infrastructure Secrets only (redacted in `==> RUN` env lines and stream logs)
- `env` — platform Variables + pipeline inline `env:` (shown in clear)
- `inputs` — trigger params (masking values decrypted at claim for execution)
- `masking_input_keys` — subset of input names to redact like secrets in logs / job UI snapshot

`masking: true` values are stored encrypted in `jobs.trigger_params` (`buzenc:v1:` + AES-GCM). Job GET never returns ciphertext or plaintext for those keys (`***` only).

---

## Pipeline Secrets Table

| Column | Type | Notes |
|--------|------|-------|
| pipeline_id | TEXT FK → pipelines.id | ON DELETE CASCADE |
| secret_id | TEXT FK → secrets.id | ON DELETE CASCADE |
| UNIQUE(pipeline_id, secret_id) | | Prevents duplicate bindings |

Bindings are created when:

1. **Form**: Advanced → **Authorized Secrets**
2. **YAML Scan / Import**: Names in `secrets:` resolved to IDs
3. **API**: `POST` / `PUT` pipelines with `secret_ids`

---

## UI Fields

### New / Edit Secret

| Field | Purpose |
|-------|---------|
| **Name** | Env var key / reference name |
| **Team** | Required ownership |
| **Team share** | Opt-in auto-inject for same-team pipelines (default off) |
| **Value** | AES-256-GCM at rest; list shows partial redaction |

### Pipeline form (Advanced)

| Field | Purpose |
|-------|---------|
| **Team** | Pipeline ownership |
| **Authorized Secrets** | Explicit bindings for non-shared (or extra) secrets |
| **Variables** / **Env** | Non-secret config |

Help text: only secrets with **Team share** enabled are auto-injected; otherwise bind or reference explicitly. Cross-team binding is not available.

---

## YAML ↔ Form Mapping

| YAML | Form | Behavior |
|------|------|----------|
| `secrets: [GIT_SSH_KEY, DOCKER_PASS]` | Authorized Secrets | Explicit binding → job env |
| `steps[].secrets: [GIT_SSH_KEY]` | Step secrets | Step env only; same-team name resolve |
| `steps[].registry_secret` | Registry Secret | Login only; same-team + **`kind=registry`** |
| _(no YAML field)_ | Secret → Kind | `generic` vs `registry` (UI filter + claim check) |
| _(no YAML field)_ | Secret → Team share | Tier‑1 auto-access when enabled |

---

## Registry credentials (`registry_secret`)

```yaml
registry_secret: acr-personal
```

```json
{"provider":"acr_personal","registry":"…","username":"…","password":"…"}
```

- Secret must have **`kind: registry`** (Infrastructure → Secrets → Kind = Registry).
- Prefer **`team_shared: false`** so unrelated pipelines never receive the JSON as env.
- Password encrypted at rest; API returns partial redaction only.
- Pipeline form **Registry Secret** dropdown lists only `kind=registry` secrets.
- See [Docker Engine — ACR Personal Edition](/docs/01-architecture/docker-engine/).

---

## Best Practices

1. **Default deny auto-inject** — leave Team share off unless many pipelines truly need the same `$NAME`
2. **Use Authorized Secrets / YAML `secrets:`** for pipeline-wide non-shared secrets
3. **Use step `secrets:`** for keys needed in one step (e.g. checkout)
4. **Set Kind = Registry** for private registry JSON, then use `registry_secret` — do not enable Team share for ACR personal credentials
5. **Keep team slugs consistent** — `team:` in YAML must match Infrastructure Teams
6. **Create Secrets in Infrastructure first**, then bind or reference by name

---

## Future: Phase 3+

| Phase | Capability |
|-------|-----------|
| Phase 1 | `owner_group` plain text, three-tier resolution, single admin |
| Phase 2 (**current**) | Teams + roles; required `team_id`; **`team_shared` opt-in**; **`kind` (generic \| registry)** |
| Phase 3 | SSO, API tokens, cross-team grants |

---

## Related

- [Users, Teams & RBAC](/docs/01-architecture/rbac/)
- [Database Schema](/docs/02-development/database-schema/)
- [Pipeline Format](/docs/01-architecture/pipeline-format/)
