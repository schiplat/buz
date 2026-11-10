---
title: Secrets & Variables
weight: 40
description: "Declare, resolve, and consume secrets and platform variables — allowlist flow, expression cheat sheet, end-to-end patterns"
aliases:
  - /docs/01-architecture/secrets-access/
  - /docs/01-architecture/platform/secrets-access/
  - /docs/01-architecture/secrets-vars-expressions/
  - /docs/01-architecture/authoring/secrets-access/
  - /docs/01-architecture/authoring/secrets-vars-expressions/
---

Canonical reference for everything about **secrets and platform variables** in
pipeline YAML: the allowlist flow, every expression syntax, and end-to-end
patterns. Buz uses the same **allowlist → explicit map → consume** flow as
GitHub Actions, with expression syntax `${secrets.X}` / `${vars.X}` (not `${{ }}`).

Related deep-dives:

- [Secrets Access Control](../platform/secrets-access-control.md) — who can use what: kind, `team_shared`, bindings
- [Pipeline Format](./pipeline-format.md) — field catalogue
- [Migrating from the kernel `config:` block](./migration-config-block.md) — legacy render/delivery
- [YAML-First Authoring](./yaml-first-authoring.md) — file authority

---

## Core rules

1. **Allowlist / reference ≠ inject.** Listing a name (or referencing `${secrets.NAME}`) does **not** put `$NAME` into the process environment by itself.
2. **Map before use.** Put values into the environment with `env:`. Registry auth and config delivery both use plugin templates (`buz/docker-login`, `buz/config-fetch`) instead of purpose slots — the kernel purpose slots were removed in the 2.x retirement.
3. **Scripts use shell vars.** After `env: GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}`, `run` uses `$GIT_SSH_KEY`. Do not write `${secrets.…}` inside `run` — the engine does not expand secrets inside scripts.
4. **Render templates get their values via `buz/env-render`'s `values` map.** One `NAME=${secrets.NAME}` line per referenced secret in `with:` — the kernel `config.render` block still works for existing pipelines, but the plugin is the recommended path.
5. **Team-shared secrets** are auto-**allowlisted** for expression resolution; they are still not auto-injected as env vars.
6. **Top-level `secrets:` is optional** for references the validator can see — step `env:` maps and `uses:` steps' `with:` inputs are collected automatically (GHA-style). It remains **required** when a `config.render` template references secrets: those references live inside external template files no YAML scan can see, so the declaration is the injection allowlist for external templates.

---

## Mental model (3 layers)

```
① DECLARE / REFERENCE (allow)   →  ${secrets.NAME} in env/slot  |  secrets: [NAME]  |  team_shared
② RESOLVE (expression, non-run) →  ${secrets.NAME}   /  ${vars.NAME}
③ CONSUME (shell / process)     →  $KEY              /  $INPUT_*
```

| Layer | What it does | Injects env? |
|-------|----------------|--------------|
| **Declare / reference** | Permission to resolve a secret name | **No** |
| **Resolve** | Expand value in YAML fields or templates | Into the mapped key / slot / file |
| **Consume** | Script or process reads env | Already in environment |

**Never confuse** `secrets: [OSS_CONFIG]` (optional list) with `${secrets.OSS_CONFIG}` (the actual value reference).

### What counts as an allowlist (any one is enough, same team)

| Source | Effect |
|--------|--------|
| Use-site `${secrets.NAME}` reference | **Implicit allowlist** for that name (same team) |
| Step `secrets: [NAME]` | Explicit step allowlist (optional narrowing) |
| Pipeline top-level `secrets:` | Optional overview + `pipeline_secrets` bind |
| `team_shared: true` | Auto-allowlisted for every same-team job |

Aligned with GitHub Actions day-to-day authoring, ordinary workflows do not
maintain a YAML secret inventory. Two styles:

| Style | YAML | When to use |
|-------|------|-------------|
| **Thin top / thick step (preferred for production)** | Omit pipeline `secrets:` — reference at the use site | DRY; closest to GHA |
| **Thick top (optional overview)** | List names under pipeline `secrets:` | Teaching examples, review footprint, UI "Authorized Secrets" mirror |

The Manager **collects** names from use-site references, step lists, and the
pipeline list when upserting YAML, so Authorized Secrets / `pipeline_secrets`
stay populated even when the top-level list is omitted.

Cross-team names always fail. Missing same-team secret → claim / expand error.

---

## Syntax inventory

### A. Declaration (YAML structure)

| Syntax | Where | Meaning |
|--------|--------|---------|
| `secrets:` list | Pipeline top-level | **Optional** job overview / bind — **not required** if steps reference secrets |
| `secrets: [NAME]` | Step | Optional explicit allowlist (also implied by `${secrets.NAME}` on that step) |
| `variables:` list | Pipeline top-level | Bind platform Variables (Infrastructure → Variables) by name |
| `env: KEY: value` | Pipeline or step | Set / map process env (`value` may contain expressions) |
| `on.manual.inputs:` | Trigger | Declare trigger params (become `$INPUT_*` in `run`) |

Platform Variables bound via `variables:` **are** injected as `$NAME` into every step (non-sensitive, visible in logs).

### B. Expressions (non-`run` only)

| Expression | Domain | Typical sources |
|------------|--------|-----------------|
| `${secrets.NAME}` | Secrets | Manager Secrets (UPPER_SNAKE_CASE), must be allowlisted (or `team_shared`) |
| `${vars.NAME}` | Vars | Pipeline `env:` keys + trigger inputs (`NAME` and/or `INPUT_NAME`) |
| `${vars.NAME:-default}` | Vars | Same; fallback if missing (render / expand) |
| `${secrets.NAME:-default}` | Secrets | Allowed by expander; prefer fail-fast in render templates |

**Where expressions are expanded:**

| Location | `${secrets.X}` | `${vars.X}` | Notes |
|----------|----------------|-------------|-------|
| `env:` values (pipeline / step) | ✅ | ✅ | Expanded at job claim → process env |
| `notify[].webhook` | ✅ | — | Resolved by Manager at send time (not claim) |
| `config.render` template files | ✅ | ✅ | Runner Strict mode; missing → fail |
| **`run:` scripts** | ❌ | ❌ | **Not expanded** — use `$KEY` / `$INPUT_*` |

The kernel purpose slots (`registry_secret`, `config.source`) were removed in the 2.x retirement — use `env:` mappings; registry login/config delivery are plugin templates (`buz/docker-login`, `buz/config-fetch`).

### C. Shell / process consumption (`run:` and engines)

| Syntax | Source | Sensitive? |
|--------|--------|------------|
| `$KEY` | After `env: KEY: …` (literal or `${secrets…}` / `${vars…}`) | If value came from a secret → masked |
| `$VAR_NAME` | Platform `variables:` binding | No — appears in logs |
| `$INPUT_VERSION` | Trigger input `version` | Optional (`masking: true`) |
| `$INPUT_GIT_*` | Git webhook adapter | See [Git Webhook Triggers](./git-webhook-triggers.md) |
| `$BUZ_CONFIG_DIR` | Engine after `config.dest` | No |
| `$BUZ_ENV_CHANGED` / `$BUZ_ENV_CHANGED_KEYS` | After `config.render` | No |

### D. Special field: `host:` / `host_labels:`

| Syntax | Meaning |
|--------|---------|
| `host: prod-server-01` | Literal Host name (Infrastructure → Hosts) |
| `host: $deploy_host` | Trigger input **`deploy_host`** (bare `$name`, **not** `$INPUT_DEPLOY_HOST`) |
| `host: [$a, $b]` | Multiple hosts / expressions |
| `host_labels: [prod, web]` | Label union with `host:` |

In **`run:`**, the same trigger param is `$INPUT_DEPLOY_HOST`.

```yaml
on:
  manual:
    inputs:
      deploy_host:
        description: Buz Host name
        required: true

steps:
  - name: Deploy
    engine: ssh
    host: $deploy_host          # field expansion — input name
    run: echo "host was $INPUT_DEPLOY_HOST"   # script — INPUT_ prefix
```

---

## End-to-end patterns

### 1) Secret → process env → script

```yaml
secrets:
  - GIT_SSH_KEY

steps:
  - name: Clone Private Repo
    secrets: [GIT_SSH_KEY]                 # ① allowlist (optional if pipeline-listed)
    env:
      GIT_SSH_KEY: ${secrets.GIT_SSH_KEY} # ② map
    run: |
      echo "$GIT_SSH_KEY" > /tmp/key && chmod 600 /tmp/key   # ③ consume
      GIT_SSH_COMMAND="ssh -i /tmp/key -o StrictHostKeyChecking=no" \
        git clone --depth 1 git@github.com:org/private.git /tmp/private
      rm -f /tmp/key
```

### 2) Secret → `.env` on the target via the `buz/env-render` plugin (recommended)

```yaml
steps:
  - name: Render env
    uses: buz/env-render@1.0.0
    engine: ssh
    host: $deploy_host
    with:
      dest_dir: /opt/app
      template: env.example   # file uses ${secrets.POSTGRES_PASSWORD}
      output: .env
      mode: "0600"
      on_missing: fail
      values: |
        POSTGRES_PASSWORD=${secrets.POSTGRES_PASSWORD}
```

The legacy kernel `config.render` block behaves the same for existing
pipelines — see [Migrating from the kernel `config:` block](./migration-config-block.md).

### 3) Platform Variables (auto env)

```yaml
variables:
  - BUILD_CACHE_DIR          # ① bind → injected as $BUILD_CACHE_DIR

steps:
  - name: Build
    run: echo "cache=$BUILD_CACHE_DIR"   # ③ no ${vars.} needed in run
```

Optional expression form in `env:` / render:

```yaml
env:
  CACHE: ${vars.BUILD_CACHE_DIR}
```

### 4) Pipeline inline env (non-secret)

```yaml
env:
  APP_NAME: myapp
  LOG_LEVEL: info

steps:
  - name: Echo
    run: echo "$APP_NAME $LOG_LEVEL"
```

---

## Quick decision table

| I want to… | Write |
|------------|--------|
| Let this job use secret `X` | Pipeline or step `secrets: [X]` (or Team share) |
| Put secret `X` into `$X` for scripts | `env: X: ${secrets.X}` |
| Log in to a private registry | `buz/docker-login` plugin template (`registry` / `username` / `password` via `${secrets.X}`) |
| Fetch OSS config bundle | `buz/config-fetch` plugin step (`url` input = `${secrets.CONFIG_BUNDLE_URL}` presigned) |
| Put secret into a generated `.env` | `buz/env-render` template line `${secrets.X}` + `values:` map |
| Bind a platform Variable | `variables: [NAME]` → use `$NAME` in `run` |
| Use Variable in a template | `${vars.NAME}` |
| Pass trigger `version` into script | `$INPUT_VERSION` |
| Select SSH host from trigger | `host: $deploy_host` (+ declare `inputs.deploy_host`) |
| Reference secret inside `run:` | **Don't** — map with `env:` first, then `$KEY` |

---

## What NOT to write

| Anti-pattern | Why |
|--------------|-----|
| `secrets: [X]` and expect `$X` in `run` | Allowlist ≠ inject |
| `${secrets.X}` inside `run:` | Engine does not expand secrets in scripts |
| `${vars.X}` inside `run:` | Same — use `$KEY` / `$INPUT_*` |
| `host: $INPUT_DEPLOY_HOST` | `host:` uses `$input_name`, not `$INPUT_*` |
| `${{ secrets.X }}` (GitHub Actions braces) | Buz uses `${secrets.X}` only |
| Secret names that are not `UPPER_SNAKE_CASE` | Create API enforces UPPER_SNAKE_CASE |

---

## Name namespaces (collision notes)

| Kind | Naming | Process env key |
|------|--------|-----------------|
| Secret | `UPPER_SNAKE_CASE` (unique) | Only after `env: KEY: ${secrets.NAME}` — usually `KEY == NAME` |
| Platform Variable | UI name | `$NAME` after `variables:` bind |
| Pipeline / step `env` | Any valid env key | `$KEY` |
| Trigger input | YAML key (e.g. `version`) | `$INPUT_VERSION` in `run`; `$version` in `host:` |
| Builtins | `BUZ_*` | Set by engine |

**Precedence** when the same env key is set from multiple non-secret sources:

```
Step env  >  Pipeline env  >  Platform variables
```

`$INPUT_*` stays in its own prefix — no overlap with secrets/vars names by convention.

---

## Allowlist & Team share (short)

| Secret `team_shared` | Effect on expressions |
|----------------------|------------------------|
| `false` (default) | Must appear in pipeline/step `secrets:` (or the resolve path of a plugin input) |
| `true` | Auto-allowlisted for same-team jobs — **still** needs `${secrets.X}` / `env:` to inject |

Cross-team secrets are forbidden. Delivery-side details:
[Secrets Access Control](../platform/secrets-access-control.md).

---

## UI checklist

| UI place | Maps to |
|----------|---------|
| Infrastructure → Secrets | Create `NAME` (value encrypted) |
| Infrastructure → Variables | Create platform vars |
| Pipeline YAML `secrets:` / Authorized Secrets | Allowlist / bind |
| Pipeline YAML `variables:` | Bind platform vars |
| Trigger dialog | Fill `on.manual.inputs` → `$INPUT_*` / `host: $name` |
| Online YAML editor completions | Suggests `${secrets.` / `${vars.` + known names |

---

## See also

- Example pipelines: `example/pipelines/oss-config-demo.yml`, `acr-personal-push.yml`, `git-clone-build.yml`, `demo-app-deploy.yml`
- UI Quick YAML Reference tab inside **New / Edit Pipeline** dialog
