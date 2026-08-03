---
title: Secrets & Variables — Expression Cheat Sheet
weight: 3
description: "All YAML / script / template ways to declare and reference secrets, platform variables, env, and trigger inputs"
---

# Secrets & Variables — Expression Cheat Sheet

**Canonical quick reference** for every way Buz expresses secrets and variables.
Use this page when authoring YAML or reviewing Quick YAML Reference.

Related deep-dives:

- [Secrets Access (GHA-style)](./secrets-access.md) — allowlist → map → consume
- [Secrets Access Control](./secrets-access-control.md) — kind, `team_shared`, bindings
- [Env Render](./env-render.md) — `${secrets.X}` / `${vars.X}` in `.env` templates
- [Pipeline Format](./pipeline-format.md) — field catalogue
- [Config Block](./config-block.md) — `config.source` / `config.render`

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

### Top-level `secrets:` is optional (GHA-style)

Aligned with GitHub Actions day-to-day authoring: **ordinary workflows do not maintain a YAML secret inventory**. Buz follows the same feel:

| Style | YAML | When to use |
|-------|------|-------------|
| **Thin top / thick step (preferred for production)** | Omit pipeline `secrets:` — reference at the use site | DRY; closest to GHA |
| **Thick top (optional overview)** | List names under pipeline `secrets:` | Teaching examples, review footprint, UI “Authorized Secrets” mirror |

**What counts as an allowlist (any one is enough, same team):**

1. **Use-site reference** — `env: KEY: ${secrets.NAME}`, `registry_secret: ${secrets.NAME}`, `config.source: ${secrets.NAME}`, or `notify[].webhook: ${secrets.NAME}`
2. **Step list** — `secrets: [NAME]` (optional narrowing / explicitness)
3. **Pipeline list** — top-level `secrets: [NAME]` (optional overview + DB bind)
4. **Team share** — secret `team_shared: true` (auto-allowlisted for the team)

The Manager **collects** names from (1)+(2)+(3) when upserting YAML so Authorized Secrets / `pipeline_secrets` stay populated even when the top-level list is omitted.

Allowlist / reference alone does **not** create `$OSS_CONFIG` / `$GIT_SSH_KEY` — still map via `env:` or a purpose slot.

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
| `registry_secret:` | ✅ | — | Purpose slot (registry kind) |
| `config.source:` | ✅ | — | Purpose slot (OSS auth) |
| `notify[].webhook` | ✅ | — | Resolved by Manager at send time (not claim) |
| `config.render` template files | ✅ | ✅ | Runner Strict mode; missing → fail |
| `config.key_path` | lenient / legacy | lenient | Prefer explicit vars; legacy `$tag` still accepted in places |
| **`run:` scripts** | ❌ | ❌ | **Not expanded** — use `$KEY` / `$INPUT_*` |

Bare names in purpose slots (`registry_secret: ACR_PERSONAL`) remain accepted for migration; **prefer** `${secrets.NAME}`.

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
    uses: ssh
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
  - name: Clone
    secrets: [GIT_SSH_KEY]                 # ① allowlist (optional if pipeline-listed)
    env:
      GIT_SSH_KEY: ${secrets.GIT_SSH_KEY} # ② map
    run: |
      echo "$GIT_SSH_KEY" > /tmp/key       # ③ consume
```

### 2) Secret → purpose slot (no `$NAME` in script)

```yaml
secrets:
  - ACR_PERSONAL
  - OSS_CONFIG

steps:
  - name: Push
    uses: shell
    secrets: [ACR_PERSONAL]
    registry_secret: ${secrets.ACR_PERSONAL}   # ② slot
    run: docker push example.com/app:$INPUT_TAG

  - name: Deliver
    uses: ssh
    host: $deploy_host
    secrets: [OSS_CONFIG]
    config:
      source: ${secrets.OSS_CONFIG}            # ② slot
      key_path: config_tpl/app/v1
      dest: /opt/app
    run: ls "$BUZ_CONFIG_DIR"
```

### 3) Secret / var → `.env` template (render)

```yaml
# env.example on the target (under config.dest):
#   DB_PASSWORD=${secrets.POSTGRES_PASSWORD}
#   IMAGE_TAG=${vars.INPUT_TAG}
#   REGISTRY=${vars.REGISTRY:-registry.example.com}

steps:
  - name: Render
    uses: ssh
    host: $deploy_host
    secrets: [POSTGRES_PASSWORD]
    config:
      dest: /opt/app
      render:
        - template: env.example
          output: .env
          mode: "0600"
          on_missing: fail
```

### 4) Platform Variables (auto env)

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

### 5) Pipeline inline env (non-secret)

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
| Use registry login secret | `registry_secret: ${secrets.X}` (+ allowlist) |
| Fetch OSS config bundle | `config.source: ${secrets.X}` (+ allowlist) |
| Put secret into a generated `.env` | Template line `${secrets.X}` + `config.render` |
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
| `false` (default) | Must appear in pipeline/step `secrets:` (or purpose-slot resolution path) |
| `true` | Auto-allowlisted for same-team jobs — **still** needs `${secrets.X}` / `env:` to inject |

Cross-team secrets are forbidden. Details: [Secrets Access Control](./secrets-access-control.md).

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

- Example pipelines: `example/pipelines/oss-config-demo.yml`, `acr-personal-push.yml`, `git-clone-build.yml`, `super-manager-deploy.yml`
- UI Quick YAML Reference tab inside **New / Edit Pipeline** dialog
