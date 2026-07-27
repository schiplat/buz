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

env:                                   # optional — static env vars
  APP_NAME: myapp

secrets:                               # optional — secret names to inject
  - DOCKER_PASSWORD
  - DEPLOY_KEY

steps:                                 # required — at least one
  - name: Checkout
    run: git pull origin main

  - name: Test
    run: cargo test
    continue-on-error: true

  - name: Build
    run: docker build -t ${APP_NAME}:${INPUT_VERSION} .
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
      docker pull ${APP_NAME}:${INPUT_VERSION}
      docker-compose up -d
    continue-on-error: true
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

Static environment variables injected into every step. Values support `${VAR}` minijinja interpolation.

```yaml
env:
  APP_NAME: myapp
  REGISTRY: docker.io
```

---

### `secrets`

List of secret names to inject. The runner fetches these from the secrets store and exposes them as environment variables inside steps. Secret values are **never logged**.

```yaml
secrets:
  - DOCKER_PASSWORD
  - DEPLOY_KEY
```

> **Note:** Secrets are managed in the Manager UI under Infrastructure → Secrets. The runner fetches the value at dispatch time.

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

**Required.** The actual command(s) to execute. Can be a single line or a multi-line block (YAML `|`). Template variables (`${INPUT_*}` and secrets) are substituted before execution.

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
| `docker` | **Docker** — runs inside a Docker container on the runner |

| | |
|---|---|
| YAML | `uses: ssh` |
| Form | Engine dropdown (Runner Shell / SSH / Docker) |

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

## Template Variables

Commands are rendered in two passes:

1. `${{ secrets.NAME }}` — replaced with the secret value fetched from the secrets store (never logged)
2. `${INPUT_<KEY>}` — replaced with the trigger parameter value, uppercased (e.g. parameter `version` → `$INPUT_VERSION`)

Usage:

```yaml
steps:
  - name: Deploy
    run: |
      docker tag myapp:${{ secrets.DOCKER_TAG }} myapp:${INPUT_VERSION}
      docker push myapp:${INPUT_VERSION}
```

---

## Execution Engines

| Value | Engine | Description |
|-------|--------|-------------|
| _(omitted)_ | `local` | Run on runner host's shell (default) |
| `ssh` | `ssh` | Execute on remote host(s) via SSH (russh). Requires `host` and/or `host_labels`. |
| `docker` | `docker` | Execute in Docker container on the runner |
| `wasm` | `wasm` | Execute in Wasm sandbox |

---

## Form ↔ YAML Mapping

When using the "Import to Form" feature after validating YAML, all fields are automatically populated. The table below shows the exact mapping:

| YAML Field | Form Field | Notes |
|------------|------------|-------|
| `name` | Name | |
| `description` | Description | |
| `on.manual.inputs` | Params | Name, Description, Required, Default per param |
| `on.webhook.path` | Webhook Config → URL Path | Only when trigger is webhook |
| `on.cron.schedule` | Cron Config → Schedule | Only when trigger is cron |
| `runner.labels` | Runner Labels | Comma-separated in form |
| `runner.timeout` | Runner Timeout | Preserves original format (e.g. `60m`) |
| `concurrency` | Concurrency Group | |
| `steps[].name` | Step Name | |
| `steps[].run` | Command | Multi-line preserved |
| `steps[].uses` | Engine | Local/SSH/Docker |
| `steps[].host` | SSH Host | Comma-separated for multiple |
| `steps[].host_labels` | Host Labels | Comma-separated for multiple |
| `steps[].work_dir` / `steps[].dir` | Work Dir | Both YAML keys accepted |
| `steps[].timeout` | Timeout | |
| `steps[].retry` | Retry | |
| `steps[].continue-on-error` | Continue on error | Checkbox |
