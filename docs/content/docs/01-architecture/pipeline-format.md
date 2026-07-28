---
title: Pipeline Format
weight: 3
description: "Full reference for Buz pipeline YAML definitions and form fields"
---

Buz pipeline definitions follow a simplified GitHub Actions-style format with sequential step execution.

## Quick Reference

```yaml
name: Deploy Production               # required
description: Build and deploy to prod # optional
team: default                         # optional team slug (defaults to "default")

on:                                    # optional — defaults to manual trigger
  manual:
    inputs:
      version:
        description: Version to deploy
        required: true
        default: latest
  cron:
    schedule: "0 2 * * *"
  webhook:
    path: deploy-prod

concurrency: deploy-prod              # optional — prevent concurrent runs

runner:                               # optional
  labels: [prod, linux-x64]
  timeout: 60m

env:                                   # [pipeline-level] inline env vars
  APP_NAME: myapp

variables:                             # [pipeline-level] platform variables
  - BUILD_CACHE_DIR

secrets:                               # [pipeline-level] secrets (all steps)
  - DOCKER_PASSWORD
  - DEPLOY_KEY

steps:                                 # required — at least one
  - name: Checkout
    run: git pull origin main

  - name: Test
    run: cargo test
    continue-on-error: true

  - name: Build
    run: docker build -t $APP_NAME:$INPUT_VERSION .
    work_dir: ./src
    timeout: 10m
    retry: 2

  - name: Deploy
    uses: ssh
    host:
      - prod-server-01
      - prod-server-02
    host_labels: [prod, web]
    run: |
      docker pull $APP_NAME:$INPUT_VERSION
      docker-compose up -d
    continue-on-error: true

  - name: Clone Private Repo
    secrets: [GIT_SSH_KEY]            # [step-level] secret — only this step
    run: |
      echo "$GIT_SSH_KEY" > /tmp/key && chmod 600 /tmp/key
      GIT_SSH_COMMAND="ssh -i /tmp/key" git clone git@github.com:org/repo.git
      rm -f /tmp/key
    timeout: 5m
```

---

## Pipeline-Level Fields

### `name`

**Required.** Unique name for this pipeline. Displayed in the UI and used as a label in builds.

| | |
|---|---|
| YAML | `name: Deploy Production` |
| Form | Name input |
| Required | Yes |

---

### `description`

Optional description shown in the pipeline list and detail pages.

| | |
|---|---|
| YAML | `description: Build and deploy to prod` |
| Form | Description input |
| Required | No |

---

### `on` (Trigger)

Defines how the pipeline is triggered. If omitted, defaults to manual trigger without dynamic parameters.

#### `on.manual` (Manual)

A user clicks "Build Now" in the UI or calls the trigger API.

```yaml
on:
  manual:
    inputs:               # optional dynamic parameters
      version:
        description: Version to deploy
        required: true
        default: latest
```

- **`inputs`** — Optional key-value map of **dynamic parameters** (Params). Each parameter name becomes `$INPUT_<KEY>` inside step commands (uppercased).
  - `description` — Optional description shown in the trigger dialog
  - `required` — If `true`, the trigger dialog marks this field with a red `*` and the user should fill it before triggering
  - `default` — Fallback value when the user leaves the field blank

> **Form behavior:** If a pipeline has `manual.inputs` defined, clicking "Build Now" opens the trigger dialog. If no inputs are defined, the pipeline is triggered immediately without a dialog.

#### `on.webhook` (Webhook)

Accepts POST requests at `/api/v1/hooks/{path}`.

```yaml
on:
  webhook:
    path: deploy-prod      # URL path segment — /api/v1/hooks/deploy-prod
```

| | |
|---|---|
| YAML | `webhook.path: my-pipeline` |
| Form | Webhook Config → URL Path |

#### `on.cron` (Scheduled)

Runs on a cron schedule (manager-side timer).

```yaml
on:
  cron:
    schedule: "0 2 * * *"   # standard 5-field cron: minute hour day month weekday
```

| | |
|---|---|
| YAML | `cron.schedule: "0 2 * * *"` |
| Form | Cron Config → Schedule (cron expression) |

---

### `concurrency`

Optional string that acts as a **concurrency group key**. When set, only **one job** with this key can be `pending` or `running` at a time. Triggering again while a job is active returns a `Conflict` error.

**Use case:** Prevent two deploys to the same environment from overlapping.

| | |
|---|---|
| YAML | `concurrency: prod` |
| Form | Concurrency Group input |
| Required | No |
| Examples | `prod`, `staging`, `ci` |

---

### `team`

Team **slug** this pipeline belongs to (Phase 2 RBAC). Alias: `owner_group`.

The YAML scanner resolves the slug to `teams.id`. If omitted or unknown, it falls back to the bootstrapped `default` team.

| | |
|---|---|
| YAML | `team: backend` (or `owner_group: backend`) |
| Form | Advanced → **Team** (required dropdown) |
| Required | Yes at runtime (`team_id`); YAML may omit and get `default` |
| See also | [Users, Teams & RBAC](/docs/01-architecture/rbac/) |

---

### `runner`

Controls which runners can pick up jobs for this pipeline.

```yaml
runner:
  labels: [linux-x64, docker] # runner must match ALL specified labels to claim a job
  timeout: 60m                # maximum wall-clock time for the entire pipeline run
```

#### `runner.labels`

Comma-separated list in the form, YAML array in the file. The manager dispatches jobs only to runners whose own labels contain **all** of these (intersection match).

| | |
|---|---|
| YAML | `labels: [linux-x64, docker]` |
| Form | Runner Labels (comma-separated, e.g. `linux-x64, docker`) |

#### `runner.timeout`

Maximum duration for the **entire pipeline run**. Expressed as `<number><s|m|h>` in YAML, parsed to seconds internally. If the sum of all step durations exceeds this, the job is forcibly killed.

| Format | Example | Seconds |
|--------|---------|---------|
| `?s` | `300s` | 300 |
| `?m` | `60m` | 3600 |
| `?h` | `1h` | 3600 |

| | |
|---|---|
| YAML | `timeout: 60m` |
| Form | Runner Timeout (e.g. `60m`) |
| Default | 3600 (1 hour) if omitted |

---

### `env`

Pipeline-level **user-defined** key-value pairs injected as environment variables into **every step**. For non-sensitive configuration specific to this pipeline.

```yaml
env:
  APP_NAME: myapp
  LOG_LEVEL: info
```

| | |
|---|---|
| **Level** | Pipeline — visible to all steps |
| **Scope** | Every step |
| **Sensitive** | No — values appear in plain text in step logs |
| YAML | `env: { KEY: value }` |
| Form | Env section — key-value pair editor |

---

### `variables`

Platform-level **non-sensitive** key-value pairs defined in the Manager UI under Infrastructure → Variables. Pipelines **bind** to them by name — via YAML or the form's multi-select dropdown. Injected as env vars into **every step**.

```yaml
# YAML (scanned pipelines) — names must exist in the platform_vars table
variables:
  - BUILD_CACHE_DIR
  - NOTIFY_EMAIL
```

| | |
|---|---|
| **Level** | Pipeline — visible to all steps |
| **Scope** | Every step |
| **Sensitive** | No — values appear in step logs |
| YAML | `variables: [NAME1, NAME2]` — names must match existing platform variables |
| Form | Variables multi-select dropdown |

> **Import behavior:** If a variable name in YAML does not exist in the Manager's Variables page, the YAML scanner silently skips it. During **Import to Form**, a warning alert is shown listing each unresolved name.

---

### `secrets`

Sensitive values (passwords, tokens, SSH keys) defined in the Manager UI under Infrastructure → Secrets. Pipelines bind to them by name.

**Pipeline-level secrets** are injected into **every step**. Their values are **never logged** and are masked in terminal output.

```yaml
# Pipeline-level — visible to all steps
secrets:
  - DOCKER_PASSWORD
  - SLACK_WEBHOOK_URL
```

**Step-level secrets** are scoped to a **single step only** — other steps cannot see them:

```yaml
steps:
  - name: Clone Private Repo
    secrets: [GIT_SSH_KEY]   # only this step sees GIT_SSH_KEY
    run: git clone git@github.com:org/repo.git
  - name: Build
    run: cargo build         # GIT_SSH_KEY NOT available here
```

| | |
|---|---|
| **Level** | Pipeline-level: all steps. Step-level: single step only. |
| **Scope** | Pipeline-level → every step. Step-level → that step only. |
| **Sensitive** | Yes — values **never logged**, masked in terminal |
| YAML (pipeline) | `secrets: [NAME1]` — names must match existing secrets |
| YAML (step) | `secrets: [NAME1]` under a step — scoped to that step |
| Form (pipeline) | Secrets multi-select dropdown |
| Form (step) | Not yet supported — the form shows a warning on import |

> **Import behavior:** Same as variables — unresolved names produce a warning alert during Import to Form.

---

## Variable & Secret Hierarchy

Every placeholder in a pipeline command falls into one of four categories, each with its own **level** (where it's defined) and **scope** (who can see it). This table is the definitive reference:

| Placeholder Type | Declared In | Level | Scope | Sensitive | Reference Syntax |
|---|---|---|---|---|---|
| `env` | Top-level `env:` block | **Pipeline** | All steps | No | `$KEY` or `$key` |
| `variables` | Top-level `variables:` / Manager UI | **Pipeline** | All steps | No | `$VAR_NAME` |
| `secrets` (pipeline) | Top-level `secrets:` / Manager UI | **Pipeline** | All steps | **Yes** | `$SECRET_NAME` |
| `secrets` (step) | Step-level `secrets:` | **Step** | That step only | **Yes** | `$SECRET_NAME` |
| `inputs` (dynamic params) | `on.manual.inputs:` | **Trigger** | All steps | No | `$INPUT_NAME` (uppercased) |

### Resolution Precedence

When a variable name appears in more than one source, the most specific wins:

```
Step-level secrets  >  Pipeline-level secrets  >  Platform variables  >  Inline env
        (tightest scope)                                              (widest scope)
```

- `$APP_NAME` — resolved from `env:` or `variables:` (non-sensitive), falls back to earlier sources
- `$DOCKER_PASSWORD` — if declared both pipeline-level and step-level, the **step-level** value takes precedence for that step
- `$INPUT_VERSION` — always from the trigger dialog; has **no overlap** with other types (prefixed with `INPUT_`)

---

## Template Variables

All variables are injected as **environment variables** before each step runs. Commands reference them with standard shell `$VAR` syntax:

```yaml
steps:
  - name: Deploy
    run: |
      echo "Deploying $APP_NAME version $INPUT_VERSION"
      echo "$DOCKER_PASSWORD" | docker login -u bot --password-stdin
      curl -X POST "$SLACK_WEBHOOK_URL"
```

| Reference | Source | Level |
|---|---|---|
| `$APP_NAME` | `env:` block | Pipeline |
| `$BUILD_CACHE_DIR` | `variables:` (platform) | Pipeline |
| `$DOCKER_PASSWORD` | `secrets:` (pipeline or step) | Pipeline / Step |
| `$SLACK_WEBHOOK_URL` | `secrets:` (pipeline) | Pipeline |
| `$GIT_SSH_KEY` | `secrets:` (step) | Step |
| `$INPUT_VERSION` | `on.manual.inputs:` | Trigger |

> **Note:** Parameter names from `on.manual.inputs` are **automatically uppercased** and prefixed with `INPUT_`. A parameter named `version` becomes `$INPUT_VERSION` inside commands.

---

## Steps

Each step is an atomic unit of work executed **sequentially** by the runner. Steps appear one after another in order.

### Step Fields

#### `name`

**Required.** Human-readable step name shown in Stage View and Build History.

| | |
|---|---|
| YAML | `name: Build and test` |
| Form | Step Name input |
| Required | Yes |

---

#### `run`

**Required.** The actual command(s) to execute. Can be a single line or a multi-line block (YAML `|`). Environment variables (from env, variables, secrets, inputs) are available as regular `$VAR` references.

```yaml
run: cargo build --release
run: |
  npm ci
  npm run build
  npm test
```

| | |
|---|---|
| YAML | `run: cargo build` |
| Form | Command textarea (multi-line) |

---

#### `uses` (Engine)

Selects the execution engine. Defaults to `local` (runner's own shell).

| Value | Description |
|-------|-------------|
| _(omitted)_ | **Local** — runs on the runner host's shell |
| `ssh` | **SSH** — connects to remote host(s) via SSH (requires `host` and/or `host_labels`) |
| `docker` | **Docker** — runs `run:` inside a container on the runner. Requires `image`. See [Docker Engine](/docs/01-architecture/docker-engine/). |

| | |
|---|---|
| YAML | `uses: ssh` |
| Form | Engine dropdown (Runner Shell / SSH / Docker) |

> **Docker** is supported (MVP). See [Docker Engine](/docs/01-architecture/docker-engine/). Enable with `engine.docker_enabled = true` on the runner.

---

#### `image`

**Docker only.** Container image reference used when `uses: docker`. Required for that engine; ignored otherwise.

```yaml
uses: docker
image: rust:1.85-bookworm
run: cargo test
```

| | |
|---|---|
| YAML | `image: node:20` |
| Form | Image (shown when Engine is Docker) |
| See | [Docker Engine](/docs/01-architecture/docker-engine/) |

---

#### `host`

**SSH only.** One or more remote host **names** registered in the Manager (Infrastructure → Hosts). Supports both a single string and a YAML array.

```yaml
# Single host
host: prod-server-01

# Multiple hosts (the step runs on each)
host:
  - prod-server-01
  - prod-server-02
```

In the form, use comma separation: `prod-server-01, prod-server-02`.

> The runner resolves host names to SSH connection details (IP, port, credentials) from the hosts table at dispatch time.

| | |
|---|---|
| YAML | `host: prod-01` or `host: [prod-01, prod-02]` |
| Form | SSH Host (comma-separated) |

---

#### `host_labels`

**SSH only.** Instead of (or in addition to) naming hosts explicitly, match hosts whose `labels` column contains these tags. Combined with `host` as a **union** (deduplicated).

```yaml
host_labels:
  - prod
  - web
```

In the form, use comma separation: `prod, web`.

This is useful when you don't want to update every pipeline YAML whenever a new host is added — just tag the host and it will be picked up automatically.

| | |
|---|---|
| YAML | `host_labels: [prod, web]` |
| Form | Host Labels (comma-separated) |

---

#### `work_dir`

Working directory for the step. The command is executed from this path. Supports both `work_dir` and `dir` in YAML (aliases).

```yaml
work_dir: ./frontend
```

| | |
|---|---|
| YAML | `work_dir: ./src` (or `dir: ./src`) |
| Form | Work Dir input |

---

#### `timeout`

Per-step timeout. Same format as `runner.timeout` (`10m`, `300s`, `1h`). If the step exceeds this duration, it is killed (and the pipeline continues or stops based on `continue-on-error`).

| | |
|---|---|
| YAML | `timeout: 10m` |
| Form | Timeout input (e.g. `10m`) |
| Default | 3600s (1 hour) |

---

#### `retry`

Number of automatic retries if the step fails. Retries happen immediately back-to-back.

| | |
|---|---|
| YAML | `retry: 2` |
| Form | Retry number input (0–5) |
| Default | 0 |

---

#### `continue-on-error`

If `true`, a failure in this step does **not** stop the pipeline — subsequent steps continue to execute. The step status is still marked as `failed` in the UI.

| | |
|---|---|
| YAML | `continue-on-error: true` |
| Form | "Continue on error" checkbox |

---

## Execution Engines

| Value | Engine | Description |
|-------|--------|-------------|
| _(omitted)_ | `local` | Run on runner host's shell (default) |
| `ssh` | `ssh` | Execute on remote host(s) via SSH (russh). Requires `host` and/or `host_labels`. |
| `docker` | `docker` | Execute `run:` in a Docker container on the runner. Requires `image`. Gated by `engine.docker_enabled`. See [Docker Engine](/docs/01-architecture/docker-engine/). |
| `wasm` | `wasm` | Execute in Wasm sandbox (not implemented) |

### Docker step fields

When `uses: docker`, YAML requires `image` (container image reference). Workspace is bind-mounted to `/buz/workspace`. Full rules: [Docker Engine](/docs/01-architecture/docker-engine/).

Example: `example/pipelines/docker-engine-smoke.yml`.

> Shelling out to `docker build` / `docker push` on the host is still a **local** (or **ssh**) step — that is not the Docker engine.

---

## Form ↔ YAML Mapping

When using the "Import to Form" feature after validating YAML, all fields are automatically populated. The table below shows the exact mapping:

| YAML Field | Form Field | Level | Notes |
|---|---|---|---|
| `name` | Name | Pipeline | |
| `description` | Description | Pipeline | |
| `on.manual.inputs` | Params | Trigger | Name, Description, Required, Default per param |
| `on.webhook.path` | Webhook Config → URL Path | Pipeline | Only when trigger is webhook |
| `on.cron.schedule` | Cron Config → Schedule | Pipeline | Only when trigger is cron |
| `runner.labels` | Runner Labels | Pipeline | Comma-separated in form |
| `runner.timeout` | Runner Timeout | Pipeline | Preserves original format (e.g. `60m`) |
| `concurrency` | Concurrency Group | Pipeline | |
| `team` / `owner_group` | Team | Pipeline | Resolves to `team_id`; form requires Team select |
| `env` | Env | Pipeline | User-defined key-value pairs, per-pipeline |
| `variables` | Variables | Pipeline | Multi-select from Infrastructure → Variables (`var_ids`) |
| `secrets` (pipeline) | Authorized Secrets | Pipeline | Multi-select from Infrastructure → Secrets (`secret_ids` → `pipeline_secrets`) |
| `secrets` (step) | _(warning on import)_ | Step | Not yet supported in form UI |
| `steps[].name` | Step Name | Step | |
| `steps[].run` | Command | Step | Multi-line preserved |
| `steps[].uses` | Engine | Step | Local/SSH/Docker |
| `steps[].image` | Image | Step | Required when Engine is Docker |
| `steps[].host` | SSH Host | Step | Comma-separated names from Infrastructure → Hosts |
| `steps[].host_labels` | Host Labels | Step | Comma-separated; union with `host` |
| `steps[].work_dir` / `steps[].dir` | Work Dir | Step | Both YAML keys accepted |
| `steps[].timeout` | Timeout | Step | |
| `steps[].retry` | Retry | Step | |
| `steps[].continue-on-error` | Continue on error | Step | Checkbox |
