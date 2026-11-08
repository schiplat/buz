---
title: Secrets Access Control
weight: 20
description: "How Buz controls which pipelines can access secrets — kind, team_shared opt-in, explicit binding, and three-tier resolution"
aliases:
  - /docs/01-architecture/secrets-access-control/
---

## Overview

Buz controls secret delivery with a **team ownership boundary** plus an **opt-in team share flag**. Every secret and pipeline belongs to a Team (`team_id`). Runtime **allowlist** (not env injection) is granted through:

1. **Team auto-allowlist** — only when the secret has `team_shared = true` and shares the pipeline’s `team_id`
2. **Explicit per-pipeline bindings** — `pipeline_secrets` / YAML `secrets:`
3. **Explicit step references** — step `secrets:` allowlist

**Getting a value into the process env** always requires an explicit map: `env: KEY: ${secrets.NAME}` (or a purpose slot). See [Secrets in Pipelines (GHA-style)](/docs/01-architecture/authoring/secrets-access/).

**Default for new secrets: `team_shared = false`.** Same-team pipelines do **not** resolve the secret unless they bind or reference it. Cross-team use remains forbidden (future Phase 3 grants).

This is **Scheme B** — chosen over “any pipeline can use any secret” to provide isolation without a full IAM product in early phases. The share flag corrects the earlier over-broad assumption that “same team ⇒ always inject.”

---

## `team_shared` (opt-in auto-allowlist)

| | |
|---|---|
| Column | `secrets.team_shared` (`INTEGER` 0/1, API `boolean`) |
| Default (create) | `false` |
| Migration (existing rows) | `false` (secure default; re-enable share or bind as needed) |
| Who may toggle | Team role with `SecretManage` (Maintainer+) |

### Semantics

| `team_shared` | Same-team pipeline behavior |
|---------------|-----------------------------|
| `false` (default) | **Not** auto-allowlisted. Use Authorized Secrets, YAML `secrets:`, step `secrets:`, or purpose slots. |
| `true` | Auto-allowlisted for `${secrets.NAME}` on every same-team job — still **not** auto-injected as `$NAME`. |

`team_shared` does **not** mean “public plaintext” or “cross-team readable.” It only means: *allow Tier‑1 expression resolution within the owning team.*

### Recommended usage

| Secret type | Suggested setting | How pipelines use it |
|-------------|-------------------|----------------------|
| Shared CI token used by many pipelines | `kind: generic`, `team_shared: true` | `env: TOKEN: ${secrets.TOKEN}` (no per-pipeline bind) |
| Prod DB password, deploy key | `kind: generic`, `team_shared: false` | Explicit bind + `env: …: ${secrets.NAME}` |

---

## `kind` (`generic` | legacy `registry`)

| | |
|---|---|
| Column | `secrets.kind` (`TEXT`, API string) |
| Values | `generic` (default) · `registry` (legacy) |
| Who may set | Team role with `SecretManage` (Maintainer+) |

| `kind` | Meaning |
|--------|---------|
| `generic` | Env inject / Authorized Secrets / step `secrets:`. |
| `registry` | Legacy: JSON registry credentials retained on existing rows. No longer creatable or validated; kernel support removed in the 2.x retirement. |

Legacy note: earlier releases backfilled decryptable values that parse as registry JSON to `kind=registry`. Such rows keep their stored kind; the kernel no longer resolves registry auth.

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

### Tier 3: Explicit name reference (step)

| Reference | Injection |
|-----------|-----------|
| Step `secrets: [NAME]` | Explicit step allowlist (optional) |
| Step / pipeline `env: KEY: ${secrets.NAME}` | Allowlists `NAME` and injects as `$KEY` |

Resolution requires the secret to exist and `secret.team_id == pipeline.team_id`. Top-level `secrets:` is optional. Name-only lookup across teams is rejected.

**Use case:** Git deploy key only on checkout; OSS config secret only for delivery.

### Resolution Order

Tiers are merged and de-duplicated by name. Explicit bindings / step maps win over auto-access when names collide.

Claim payload (Manager → Runner):

```json
{
  "job_id": "…",
  "pipeline_id": "…",
  "tasks": [ { "step_secrets": {} } ],
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

`secrets`, step secrets, and keys listed in `masking_input_keys` are redacted in logs. Plaintext `inputs` are still injected as env for execution. See [Resource Binding Map](#resource-binding-map).

### Unified log-masking entry point

"Which values / keys are sensitive" is decided in **one** place (`shared::log_masking`), so adding a new sensitive field can't be missed:

- **Manager (producer)** pushes every sensitive value into the step payload's `log_mask_values` (`SecretClass::ConfigUrl` for the signed OSS URL, …). Adding a new sensitive field = registering it here; the Runner needs no changes.
- **Runner (consumer)** builds a per-step `SecretCatalog` from `secrets` keys + `masking_input_keys` + step `secrets:` + `log_mask_values`, then calls the shared `mask_secrets` primitive. `==> RUN` echo masks by key (`catalog.is_secret_key`); streamed output is masked by value.
- **`SecretClass`** is the category registry (`Secret` / `MaskingInput` / `ConfigUrl` / `SshKey`) — extend it and the producer registration when introducing a new secret kind.
---

## Resource Binding Map

| Resource | Where created | How bound to a pipeline | Runtime injection |
|----------|---------------|-------------------------|-------------------|
| **Secrets** | Infrastructure → Secrets | (1) `team_shared` + same team, or (2) **Authorized Secrets** / YAML `secrets:`, or (3) step `secrets:` / purpose slots | Allowlist for `${secrets.NAME}`; process env only via `env: KEY: ${secrets.NAME}` (**masked**) |
| **Variables** | Infrastructure → Variables | Advanced → Variables / YAML `variables:` | Env `$NAME` (**not masked**) |
| **Hosts** | Infrastructure → Hosts | Step `host` / `host_labels` when `engine: ssh` | SSH params on runner |
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

## YAML secret bindings (authoritative)

Prefer YAML `secrets:` + `env: KEY: ${secrets.NAME}` (see [Secrets Access](/docs/01-architecture/authoring/secrets-access/)). Form create/edit for pipelines is removed ([YAML-First Authoring](/docs/01-architecture/authoring/yaml-first-authoring/)).

| YAML / UI | Behavior |
|-----------|----------|
| `secrets: [GIT_SSH_KEY, …]` (optional top-level) | Allowlist overview; does **not** inject `$NAME` alone |
| `env: KEY: ${secrets.NAME}` | Explicit map → process env |
| `steps[].secrets: [GIT_SSH_KEY]` | Optional step allowlist narrowing |
| Secrets UI → Kind | `generic` default; legacy `registry` rows render/edit-only (kernel support removed in the 2.x retirement) |
| Secrets UI → Team share | Tier‑1 auto-allowlist when enabled |

---

## Registry credentials

Registry auth is no longer a secret/pipeline feature (kernel support removed in the 2.x retirement). Log in with the `buz/docker-login` plugin template using plain `generic` secrets (`username` / `password`) — see [Docker Engine](/docs/01-architecture/execution/docker-engine/). Legacy `kind=registry` rows keep their stored kind.

---

## Best Practices

1. **Default deny auto-inject** — leave Team share off unless many pipelines truly need the same `$NAME`
2. **Use Authorized Secrets / YAML `secrets:`** for pipeline-wide non-shared secrets
3. **Use step `secrets:`** for keys needed in one step (e.g. checkout)
4. **Private registry auth** — use the `buz/docker-login` plugin template with plain username/password secrets; do not enable Team share for registry credentials
5. **Keep team slugs consistent** — `team:` in YAML must match Infrastructure Teams
6. **Create Secrets in Infrastructure first**, then bind or reference by name

---

## Verbose Mode (any user, extra detail, redaction intact)

For every user (member or admin), the trigger dialog offers **Verbose mode**: the Runner logs extra per-step execution detail while **secret redaction stays fully intact** — no plaintext values ever appear. It is the safe alternative to Debug mode for day-to-day troubleshooting.

### How it works

| Layer | Behavior |
|-------|----------|
| Trigger UI | Rendered for everyone in the trigger dialog; sends `verbose: true` in the trigger request |
| `POST /pipelines/{id}/trigger` | No admin check — `verbose` is honored for any caller; audit event records `verbose` |
| `jobs.verbose_logs` | Persisted on the job (DB column, default `0`); distinct from `debug_unredact` |
| Runner dispatch | `poll` returns `verbose` on the task payload |
| Runner logs | Per step: `==> STAGE prepare / exec` markers, `==> STEP CONFIG` (exec type, timeout, fail strategy, work dir, image, `config_dest`/`config_render`, ssh hosts as `user@host:port` — **never credentials**), `==> SSH [user@host:port]` + remote `workspace` after login, `==> STEP RESULT` (duration + exit code, including prepare-stage failures) |

### What verbose adds

- **Per-step duration** and **exit code** — `==> STEP RESULT` reports `duration` + `exit code` even on failure (including config render prepare failures).
- **Stage progress** — `==> STAGE prepare` / `==> STAGE exec` markers around each step. `STEP CONFIG` is emitted **before** prepare so a download failure still shows the step shape.
- **Command shape** — `==> RUN` already shows the multi-line expanded command; verbose additionally renders `==> STEP CONFIG` with the full step shape (metadata only).
- **Config prepare** — `==> CONFIG PREP` shows `object_key` / `expires_in` before the OSS download; the signed URL stays `***` unless Debug is also on.

### Log display order (`chunk_index`)

Job Detail and `GET /jobs/{id}/logs` render chunks with `ORDER BY step_order, chunk_index` (see `job_logs`). Live WebSocket delivery follows upload time, but after the job finishes the UI reloads from the DB — so **`chunk_index` must match the intended on-screen sequence**, not merely “whatever was free in the meta region.”

Each step owns a window `step_idx * 10_000` (`step_log_base` in `crates/runner/src/client/poll.rs`). Offsets inside that window:

| Offset | Banner | When |
|-------:|--------|------|
| 0 | `==> STAGE prepare` | Verbose |
| 1 | `==> STEP CONFIG` | Verbose (before prepare work) |
| 2 | `==> CONFIG PREP` | Verbose **or** Debug, when `config_auth` is present |
| 4 | `==> STAGE exec` | Verbose |
| 5 | `==> RUN […]` | Always (command echo) |
| 6… | Streamed stdout/stderr | Always |
| 9993 | `==> STEP RESULT` | Verbose (after stream / prepare failure) |
| 9997 | `==> ERROR` | Failure |
| 9998–9999 | Reserved meta | Internal |

Expected terminal order for a successful Verbose shell step:

```text
==> STAGE prepare
==> STEP CONFIG
  …
==> STAGE exec
==> RUN [shell]
  …
<command output>
==> STEP RESULT
  duration   …
  exit code  0
```

#### Historical bugs (fixed)

1. **Verbose markers sorted after RUN** — Prep/config/exec used offsets `9990–9994` while `RUN` was `1` and stream started at `2`. Upload order was correct (prepare → RUN → result), but DB reload showed `RUN` + output first, then `STAGE prepare` / `STEP CONFIG` / `STAGE exec` at the bottom. Fix: assign prep/config/exec offsets **before** `LOG_ECHO` / `LOG_STREAM_START` as in the table above. Old jobs keep their stored indexes; only new runs (with an updated Runner) show the corrected order.

2. **Prepare-only failures looked identical with/without Verbose** — For steps that fail during config prepare, there is no `==> RUN`. `STEP CONFIG` / `STAGE exec` are uploaded before prepare, and on prepare failure `ERROR` + verbose `STEP RESULT` upload with `exit code -1`, all with that step’s `step_order` (not job-level `-1`).

Constants and ordering tests live in `poll.rs` / `poll_tests.rs` (`verbose_chunk_order_matches_display_sequence`).

### Safety properties

- **Redaction is untouched** — `verbose` only adds meta chunks produced by the Runner from keys/metadata; `SecretCatalog` value masking still applies to all streamed output. `debug_unredact` remains the only way to ever see plaintext, and it stays admin-only.
- **Never conflicts with Debug mode** — the two flags are independent and may combine (a verbose debug run shows detail with plaintext, admin-only, 24h retention). Prefer Verbose alone for day-to-day troubleshooting; use Debug only when plaintext values or a full signed `CONFIG PREP` URL are required.
- **No new data stored** — verbose detail is derived at log time in the Runner; no extra columns beyond the `verbose_logs` job flag.

---

## Debug Mode (admin-only unredacted logs)

For troubleshooting, a **platform admin** can trigger a pipeline with **Debug mode** enabled: secret values are **not redacted** in the job logs (`==> RUN` env lines and streamed output). For steps with nested `config:` OSS deliver, Debug also prints the full signed URL in `==> CONFIG PREP` (even when Verbose is off) — that is often the only visible difference when prepare fails before any RUN echo. Pair with Verbose for stage markers. This is a deliberate, high-risk escape hatch. Non-admins cannot enable Debug (UI hidden + API rejected).

### How it works

| Layer | Behavior |
|-------|----------|
| Trigger UI | Only rendered when `auth.isPlatformAdmin`; sends `debug: true` in the trigger request |
| `POST /pipelines/{id}/trigger` | Rejects non-admins with **403** when `debug: true`; audit event records `debug_unredact` |
| `jobs.debug_unredact` | Persisted on the job (DB column, default `0`); stored in the frozen snapshot-adjacent fields |
| Runner dispatch | `poll` returns `debug_unredact` on the task payload; the Runner then tracks **no** secret values and clears all secret keys → `mask_secrets` is a no-op and `==> RUN` shows real values |
| Log read gate | `GET /jobs/{id}/logs` and the WS stream reject non-admins with **403** — debug jobs are only readable by platform admins, regardless of team membership |
| Retention | A background task (`services::debug_log_cleanup`) runs every 5 minutes and **purges `job_logs` for debug jobs older than 24 hours**, resetting `debug_unredact` so the gate + UI banner stop applying. The job row itself is kept for history/audit. The trigger dialog and the job detail page both surface the 24h retention |

### Safety properties

- **Job-scoped, one-shot** — the flag lives on the job, never as a global setting. The next normal trigger is fully redacted again.
- **Not silently downgraded** — a non-admin sending `debug: true` gets an explicit 403; the flag is never ignored.
- **Audited** — the `pipeline.trigger` audit event records `debug_unredact: true` with the triggering admin's identity.
- **Read gate** — even a team member with `TeamView` cannot open a debug job's logs.

### Risk note

Long-lived secrets (SSH keys, passwords, deploy tokens) become plaintext in `job_logs` permanently until the 24h purge. Prefer short-lived throwaway values when using Debug mode. After 24h the logs are deleted automatically and the job's `debug_unredact` flag is reset; the job record itself remains for audit.

---

## Future: Phase 3+

| Phase | Capability |
|-------|-----------|
| Phase 1 | `owner_group` plain text, three-tier resolution, single admin |
| Phase 2 (**current**) | Teams + roles; required `team_id`; **`team_shared` opt-in**; **`kind` (generic \| registry)** |
| Phase 3 | SSO, API tokens, cross-team grants |

---

## Related

- [Users, Teams & RBAC](/docs/01-architecture/platform/rbac/)
- [Database Schema](/docs/02-development/database-schema/)
- [Pipeline Format](/docs/01-architecture/authoring/pipeline-format/)
