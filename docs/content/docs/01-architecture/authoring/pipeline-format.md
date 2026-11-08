---
title: Pipeline Format
weight: 10
description: "Full reference for Buz pipeline YAML definitions (Form mapping is transitional)"
aliases:
  - /docs/01-architecture/pipeline-format/
---

Buz pipeline definitions follow a simplified GitHub Actions-style format with sequential step execution.

> **Authoring direction:** File/Git under Manager `yaml_scan_dir` is the authority; the UI online editor writes that path. Form step-card authoring has been **removed** — see [YAML-First Pipeline Authoring](./yaml-first-authoring.md). Per-field tables below may still mention a historical **Form** column; ignore it when authoring (use YAML + Definition).

## Field registration (required before use)

Validate enables `deny_unknown_fields` on **every** nesting level of the pipeline schema. A YAML keyword that is not registered on the corresponding Rust struct is **rejected** (typos are not silently ignored).

**New fields must be registered in schema before they can be used in YAML.**

| Level | Register on (`crates/shared/src/pipeline_yaml.rs`) |
|-------|-----------------------------------------------------|
| Top-level | `PipelineYaml` |
| `notify[]` | `NotifyChannelYaml` |
| `runner` | `RunnerYaml` |
| `on` / triggers / inputs | `TriggerYaml`, `ManualTriggerYaml`, `InputDef`, … |
| `concurrency` | `ConcurrencyYaml` (aliases via `#[serde(alias)]`; do not rely on untagged to swallow errors) |
| `steps[]` | `StepYaml` |
| `steps[].config` | `ConfigBlock` |
| `config.render` | `ConfigRenderEntry` (`ConfigRender` uses custom deserialize) |

Rules:

- Do **not** remove `deny_unknown_fields` or reintroduce untagged enums that hide unknown-key errors
- Legal aliases must be explicit (`alias` / `rename`) — e.g. `work_dir`→`dir`, `max-queue`→`max_queue`, `owner_group`→`team`
- Enum-like values (`engine`, `concurrency.mode`, …) must be validated in `parse_and_validate()` against an allowlist
- After schema registration, still wire storage / claim / runner / FE types / examples / docs (project checklist)

```yaml
# Rejected — `ru` is not a registered step key
steps:
  - name: x
    engine: shell
    ru: echo hi

# OK — registered key `run`
steps:
  - name: x
    engine: shell
    run: echo hi
```

```bash
cargo test -p shared --test pipeline_yaml_test
```

## Quick Reference

```yaml
name: Deploy Production               # required
environments: [staging, production]  # required registered slug allowlist; every run selects one
description: Build and deploy to prod # optional
team: default                         # optional team slug (defaults to "default")

on:                                    # optional — defaults to manual trigger
  manual:
    inputs:
      version:
        description: Version to deploy
        required: true
        default: latest
        type: string                  # optional; string is the default
        pattern: "^[A-Za-z0-9._-]+$"  # regex — validated at trigger (fail-fast)
      deploy_host:
        description: Buz Host name (must exist in Infrastructure → Hosts)
        required: true
        type: host                    # registry check at trigger
      environment_name:
        description: Business input only; deployment environment is selected separately
        type: choice
        options: [staging, production]
      replicas:
        type: number
        default: "3"
  webhook:
    path: deploy-prod
    environment: production       # optional default; caller may select another declared environment

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

notify:                                # optional — Manager sends on job terminal status
  - type: feishu
    webhook: ${secrets.FEISHU_WEBHOOK} # required; store URL in Secrets
    # on: [success, failure]           # default; add cancelled if needed

steps:                                 # required — at least one; each step needs `run`
  - name: Checkout
    run: git pull origin main          # required (typos like `ru:` are rejected)

  - name: Test
    run: cargo test
    continue-on-error: true

  - name: Build
    run: docker build -t $APP_NAME:$INPUT_VERSION .
    work_dir: ./src
    timeout: 10m
    retry: 2

  - name: Deploy
    engine: ssh
    host:
      - prod-server-01
      - prod-server-02
    host_labels: [prod, web]
    hosts_strategy: parallel          # chain (default) | parallel
    run: |
      docker pull $APP_NAME:$INPUT_VERSION
      docker-compose up -d
    continue-on-error: true

  - name: Clone Private Repo
    env:
      GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}  # [step-level] map → $GIT_SSH_KEY
    run: |
      echo "$GIT_SSH_KEY" > /tmp/key && chmod 600 /tmp/key
      GIT_SSH_COMMAND="ssh -i /tmp/key" git clone git@github.com:org/repo.git
      rm -f /tmp/key
    timeout: 5m
```

---

## Pipeline-Level Fields

Built-in top-level keys on `PipelineYaml` (`deny_unknown_fields`):

| Field | Required | Summary |
|-------|:--------:|---------|
| [`name`](#name) | ✓ | Unique pipeline name |
| `environments` | ✓ | Registered environment slugs allowed for trigger-time selection |
| [`description`](#description) | | UI description |
| [`team`](#team) / `owner_group` | | Team slug (default `default`) |
| [`on`](#on-trigger) | | Triggers: `manual` / `webhook` / `cron` |
| [`concurrency`](#concurrency) | | Concurrent-run policy |
| [`runner`](#runner) | | Label match + job timeout |
| [`env`](#env) | | Pipeline-level env map |
| [`variables`](#variables) | | Platform variable name allowlist |
| [`secrets`](#secrets) | | Optional secret name overview / allowlist |
| [`notify`](#notify) | | End-of-job notification channels |
| `steps` | ✓ | Ordered step list (see [Steps](#steps)) |

## `environments` (Required deployment targets)

Every pipeline must declare a non-empty top-level list of registered environment slugs:

```yaml
environments: [staging, production]
```

The YAML validator rejects unknown slugs. Every manual/API trigger must explicitly select one of these declared environments; missing or undeclared values are rejected. This is separate from `on.manual.inputs`: an input named `environment` is only an ordinary job parameter and does not select or authorize a deployment environment.

---

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
  - `masking` — If `true`, the value is still injected as env for execution, but:
    - Redacted (`***`) in Runner `==> RUN` env echo / step logs, job snapshot, API `trigger_params`, and `pipeline.trigger` audit
    - **Encrypted at rest** in `jobs.trigger_params` (AES-256-GCM, same key as Secrets; `buzenc:v1:` prefix). Claim decrypts for the Runner.
  - `type` — Optional value constraint, validated **at trigger time** (fail-fast, before a job is scheduled):
    - `string` (default) — free-form text
    - `number` — must parse as an integer
    - `boolean` — must be `true` or `false` (case-insensitive); rendered as a `true`/`false` select in the dialog
    - `host` — must reference a **registered Buz Host name** (validated against the Hosts registry)
    - `choice` — value must be one of the declared `options`; rendered as a dropdown in the dialog
    - `list` — string list, one item per line; exported to `INPUT_<NAME>` with newline separators (multi-target deploys). Rendered as a textarea; declared `pattern` applies to every item. No nested maps — pass structured values as a JSON string.
  - `pattern` — Optional ECMAScript-compatible regex the resolved value must match (e.g. `"^[A-Za-z0-9._-]+$"`). Works with any `type` and replaces hand-written shell `case`/regex guards in the first step.
  - `options` — Allowed values for `type: choice`.

```yaml
on:
  manual:
    inputs:
      version:
        description: Version to deploy
        required: true
      tag:
        description: ACR image tag
        required: true
        default: "0.1.0"
        type: string
        pattern: "^[A-Za-z0-9._-]+$"
      environment_name:
        description: Business input only; deployment environment is selected separately
        type: choice
        options: [staging, production]
      deploy_host:
        description: Buz Host name
        required: true
        type: host
      dry_run:
        description: Skip live changes
        type: boolean
        default: "false"
      replicas:
        description: Instance count
        type: number
        default: "3"
```

> **Validation timing:** `required` / `type` / `pattern` / `options` are enforced by the Manager on every trigger path (manual, webhook, cron) **before** the job is queued. A violation returns `422` with field-level errors (`invalid trigger inputs — tag: value does not match pattern '…'`), and the trigger dialog highlights the offending fields in red. `default` is merged when the field is omitted, then validated like any user-provided value — so a malformed default is caught at trigger, not silently injected.

```yaml
on:
  manual:
    inputs:
      version:
        description: Version to deploy
        required: true
      deploy_token:
        description: One-time token
        required: true
        masking: true
```

> **`masking` vs Secrets:** Infrastructure Secrets are encrypted at rest with ACL and reused across jobs. `masking: true` inputs are also encrypted in `trigger_params`, but are one-shot per job — prefer Secrets for long-lived credentials. Changing `jwt_secret` makes old sealed masking values undecryptable (same as Secrets).

> **Form behavior:** If a pipeline has `manual.inputs` defined, clicking "Build Now" opens the trigger dialog. If no inputs are defined, the pipeline is triggered immediately without a dialog.

#### `on.webhook` (Webhook)

Accepts POST requests at `/api/v1/hooks/{path}`.

```yaml
on:
  webhook:
    path: deploy-prod      # URL path segment — /api/v1/hooks/deploy-prod
    environment: production # default target when the caller omits one
    secret: DEPLOY_HOOK_KEY  # optional team secret name → HMAC verification
```

A webhook caller may choose any registered environment also declared by the pipeline. The selector may be supplied as the `environment` query parameter or in the JSON body (top-level or `inputs.environment`); precedence is query > body > this configured default. `environment` is reserved for deployment targeting and is not passed as a business input. If no selector or configured default exists, the trigger is rejected; it never picks the first declared environment.

| | |
|---|---|
| YAML | `webhook.path: my-pipeline` |
| Form | Webhook Config → URL Path |

**Optional HMAC verification** — when `secret` names a team secret, callers must sign the raw body:

```bash
SIG="sha256=$(printf '%s' '{"inputs":{"version":"1.2"}}' | openssl dgst -sha256 -hmac "$DEPLOY_HOOK_KEY" -hex | awk '{print $2}')"
curl -X POST "https://manager.example.com/api/v1/hooks/deploy-prod" \
  -H "Content-Type: application/json" \
  -H "X-Buz-Signature: $SIG" \
  -d '{"inputs":{"version":"1.2"}}'
```

Requests without a valid `X-Buz-Signature: sha256=<hex>` header are rejected with 401. Only the secret *name* is stored on the pipeline; the value is resolved (and rotated) via the team's Secrets.

> **GitHub / Gitee push triggers** do **not** add YAML fields. Configure URL + Secret in the Pipeline UI; the adapter injects `INPUT_GIT_*` at job creation. See [Git Webhook Triggers](/docs/01-architecture/authoring/git-webhook-triggers/).

#### `on.cron` (Scheduled)

Runs on a cron schedule (manager-side timer).

```yaml
on:
  cron:
    schedule: "0 2 * * *"   # standard 5-field cron: minute hour day month weekday
    environment: production # required; must be in top-level environments
```

Cron triggers require an explicit `environment` because a timer has no caller to choose one. Webhooks may configure `environment` as their default target; requests can override it with a registered slug that the pipeline declares. Neither path falls back to the first declared environment.

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
| See also | [Users, Teams & RBAC](/docs/01-architecture/platform/rbac/) |

---

### `runner`

Controls which runners can pick up jobs for this pipeline.

```yaml
runner:
  labels: [linux-x64, docker] # runner must match ALL specified labels to claim a job
  timeout: 60m                # maximum wall-clock time for the entire pipeline run
```

#### `runner.labels`

Optional. Comma-separated list in the form, YAML array in the file. The manager dispatches jobs only to runners whose own labels contain **all** of these (AND / intersection match). Empty list matches any runner.

> Labels are capability selectors (`linux-x64`, `gpu`, …), **not** a security boundary. Cross-team execution isolation is controlled by the Runner's team binding (admin-managed): a team-bound Runner only receives this team's jobs regardless of labels. Jobs always run as the Runner's OS user and are not sandboxed from one another.
>
> Keep labels capability-named (`linux-x64`, not `sandbox`): isolation tiers are Runner Policy chosen by a platform admin, and never appear in pipeline YAML. See [Runner Policy — Who decides what: the authoring boundary](/docs/01-architecture/execution/runner-concurrent-jobs/#who-decides-what-the-authoring-boundary).

| | |
|---|---|
| YAML | `labels: [linux-x64, docker]` |
| Form | Runner Labels (optional; help text explains AND) |

#### `runner.timeout`

Maximum duration for the **entire pipeline run**. Expressed as `<number>` or `<number><s|m|h>` in YAML (no spaces). Invalid values (e.g. `10x m`) fail Validate / Save. Parsed to seconds internally. If the sum of all step durations exceeds this, the job is forcibly killed.

| Format | Example | Seconds |
|--------|---------|---------|
| bare | `300` | 300 |
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

Platform-level **non-sensitive** key-value pairs defined in the Manager UI under Infrastructure → Variables. Pipelines **bind** to them by name in YAML. Injected as env vars into **every step**. Values are **not** masked in Job logs (unlike Infrastructure Secrets).

```yaml
# Names must exist in Infrastructure → Variables
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

> **Validate / Save:** unknown platform variable names are **rejected** (no silent skip). See [YAML-First Authoring](./yaml-first-authoring.md).

---

### `notify`

Optional end-of-build notifications. Full guide: **[Pipeline Notify](./pipeline-notify.md)** (Feishu setup, events, deep links, troubleshooting).

When a job reaches a **terminal** status (`success` / `failed` / `cancelled`; `timeout` counts as failure), the Manager asynchronously POSTs to configured channels. Send failures are logged only and never block the job state machine.

```yaml
notify:
  - type: feishu
    webhook: ${secrets.FEISHU_WEBHOOK}   # required — Feishu custom bot URL in Secrets
    on: [success, failure]               # optional; default [success, failure]
    # on: [success, failure, cancelled]  # include cancelled explicitly to notify on cancel
```

| Field | Required | Notes |
|-------|----------|--------|
| `type` | Yes | Channel kind. Currently only `feishu`. Unknown types fail Validate. |
| `webhook` | Yes | Must be `${secrets.NAME}` (or bare secret name). Resolved at send time from the pipeline team. |
| `on` | No | Events: `success` \| `failure` \| `cancelled`. YAML `failure` maps to job status `failed` (and `timeout`). Default `[success, failure]` — **cancelled is not included** unless listed. Empty `on: []` never sends. |

**Behavior (summary):**

- Config is YAML-only (no separate UI / DB table). Frozen into the job snapshot at trigger (expressions kept; plaintext webhook never stored in the snapshot).
- Deduped by the existing terminal-status write path (non-terminal → terminal once).
- Feishu card: status, pipeline name, job id, duration, triggered_by, plus **View Job** when [`server.public_base_url`](./pipeline-notify.md#manager-config-job-detail-deep-link) is set (`{base}/jobs/{job_id}`).

| | |
|---|---|
| YAML | `notify: [{ type, webhook, on? }]` |
| Form | — (YAML only) |
| Required | No |

---

### `secrets`

Sensitive values (passwords, tokens, SSH keys) defined in the Manager UI under Infrastructure → Secrets.

**Top-level `secrets:` is optional** (GHA-style). Prefer referencing at the use site — that reference is the allowlist. Declaring a name still does **not** put `$NAME` into the process environment.

**Required access pattern:** map via `env:` (or a purpose slot), then use the shell variable in `run`:

```yaml
# Production (thin top): omit pipeline secrets: — reference at use site
steps:
  - name: Clone Private Repo
    env:
      GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}   # allowlists + injects as $GIT_SSH_KEY
    run: |
      echo "$GIT_SSH_KEY" > /tmp/key && chmod 600 /tmp/key
      GIT_SSH_COMMAND="ssh -i /tmp/key -o StrictHostKeyChecking=no" \
        git clone git@github.com:org/repo.git /tmp/repo
  - name: Notify
    env:
      SLACK_WEBHOOK_URL: ${secrets.SLACK_WEBHOOK_URL}
    run: curl -X POST "$SLACK_WEBHOOK_URL"
```

Purpose slots use the same expression (no separate list required):


Optional top-level / step `secrets: [NAME]` remains valid as an overview or narrowing. Manager collects effective names from lists + refs when upserting YAML.

Do **not** write `${secrets.…}` inside `run` — the engine does not expand secrets in scripts. Full rules: [Secrets & Variables](/docs/01-architecture/authoring/secrets-variables/). Syntax cheat sheet: [Secrets & Variables](/docs/01-architecture/authoring/secrets-variables/).

Secrets default to **`team_shared: false`**. Same-team only; see [Secrets Access Control](/docs/01-architecture/platform/secrets-access-control/).

| | |
|---|---|
| **Level** | Use-site `${secrets.NAME}` / purpose slot = allowlist. Top-level / step `secrets:` optional. |
| **Into process env** | Only via `env: KEY: ${secrets.NAME}` (expanded at claim) |
| **Sensitive** | Yes — values **never logged**, masked in terminal |
| YAML (pipeline) | `secrets: [NAME1]` — optional overview |
| YAML (step) | `${secrets.NAME}` in `env:` / slots; optional `secrets: [NAME]` |
| Form (pipeline) | Authorized Secrets (auto from refs) + Env rows with `${secrets.NAME}` |
| Form (step) | Step Secrets + auto `env` mapping on save |

> **Validate / Save:** unknown or inaccessible secret names are **rejected** (viewer-scoped; no cross-team name oracle). See [YAML-First Authoring](./yaml-first-authoring.md).

---

## Variable & Secret Hierarchy

Every placeholder in a pipeline command falls into one of four categories, each with its own **level** (where it's defined) and **scope** (who can see it). This table is the definitive reference:

| Placeholder Type | Declared In | Level | Scope | Sensitive | Reference Syntax |
|---|---|---|---|---|---|
| `env` | Top-level / step `env:` block | **Pipeline / Step** | Mapped steps | Optional (if value from secrets) | `$KEY` after map; values may be `${secrets.X}` / `${vars.X}` |
| `variables` | Top-level `variables:` / Manager UI | **Pipeline** | All steps | No | `$VAR_NAME` |
| `secrets` (allowlist) | Top-level / step `secrets:` | **Pipeline / Step** | Expression resolution | **Yes** | `${secrets.NAME}` in `env:` / slots / render — **not** bare `$NAME` from allowlist |
| `inputs` (dynamic params) | `on.manual.inputs:` | **Trigger** | All steps | Optional (`masking: true`) | `$INPUT_NAME` (uppercased) |

### Resolution Precedence

When a variable name appears in more than one source, the most specific wins:

```
Step env map  >  Pipeline env map  >  Platform variables
```

- `$APP_NAME` — from `env:` or `variables:` (non-sensitive)
- `$GIT_SSH_KEY` — only if `env: GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}` was declared (allowlist alone is not enough)
- `$INPUT_VERSION` — always from the trigger dialog; has **no overlap** with other types (prefixed with `INPUT_`)

---

## Built-in `BUZ_*` Environment

Every step also receives read-only job metadata as `BUZ_*` environment variables — mainly so plugin/notification steps can render messages like *build #123 passed* without extra plumbing. They are never secret values and are safe to echo:

| Variable | Content |
|----------|---------|
| `BUZ_JOB_ID` | Job id (dashboard URL: `$BUZ_JOB_URL`) |
| `BUZ_JOB_URL` | Link to this job in the dashboard |
| `BUZ_PIPELINE_NAME` | Pipeline name |
| `BUZ_TEAM` | Owning team name |
| `BUZ_TRIGGER` | `manual`, `webhook`, or `cron` (pipeline trigger type) |
| `BUZ_TRIGGERED_BY` | Who triggered the job (`trigger_by`) |
| `BUZ_GIT_SHA` / `BUZ_GIT_BRANCH` | Normalized from the `GIT_SHA` / `GIT_BRANCH` webhook inputs; absent on non-git triggers |

---

## Marketplace Plugins (`uses:`) — schema preview

Steps may reference a plugin from a plugin index instead of inlining `run:`:

```yaml
steps:
  - name: notify
    uses: community/feishu@1.2     # <namespace>/<plugin-id>@<pin>
    with:                                  # inputs of the referenced template
      webhook: ${secrets.FEISHU_WEBHOOK}
      title: deploy done
```

Schema rules enforced today: `uses:` takes `<namespace>/<plugin-id>@<pin>` with pin `@1`, `@1.2`, `@1.2.0`, or `@sha256:<64 hex>` (ranges and `latest` are rejected); a `uses:` step cannot also declare `image:` (template-owned, no override); `engine:` is allowed as a deployment-context override — resolution enforces the template's declared `engines` at save time; `run:` is appended after the template run block (template = environment, call site = commands); `with:` requires `uses:`. Templates may declare an optional `category:` for browsing and filtering; it is metadata, not part of the stable ID. Templates that must run against a minimum Buz release declare `requires_buz` (see the [Plugin Contract](../plugin-contract/)).

**Resolution (local index)**: at pipeline save, each `uses:` reference is resolved against the local plugin index — `<data_dir>/plugins` by default (override with `[plugin_index].local_dir` in Manager config), one YAML spec file per plugin. Specs are discovered recursively; hidden paths and symlinks are ignored. Keep each `(id, version)` in exactly one file; `buz-plugin validate <directory>` recursively checks specs and rejects duplicate ID/version pairs. Pins lock at save time ("resolve & lock"): the resolved version, content hash, source, and expanded step JSON are stored as provenance, so upstream tag moves never change an already-saved pipeline. Save-time errors are explicit: template not found, no version matching the pin, the plugin requiring a newer Buz, or `with:` not matching the template's declared inputs. The full design (remote `https` index, static browse site, update badges) is Phase 2 of the marketplace ADR.

For Docker deployments using a custom index, mount the selected host directory into the Manager container and set `[plugin_index].local_dir` (or `BUZ_PLUGIN_INDEX__LOCAL_DIR`) to its container path. The Manager reads the local specs; it does not clone or synchronize a Git mirror. See the [configuration guide](/docs/03-operations/configuration/#external-plugin-git-mirror) for a Compose example.

### Plan a pipeline without executing it

`pipeline plan` performs the same structural validation, local `uses:` resolution, version selection, template expansion, and input validation as pipeline save, then prints a redacted plan. It does not start Manager services, open the database, write files, contact a Runner, or execute steps. Secret and variable values are never printed; only declared input names are listed. By default it also checks every Docker step's tag or digest against the registry's read-only V2 manifest API; it does not pull or run images. Docker Hub and anonymous Bearer-token registries are supported. Private images that require credentials, network failures, timeouts, and unsupported registry responses are reported as `unverified`; an explicit `not found` result fails the command. Use `--offline` to skip registry requests. This registry check only confirms that the manifest is addressable; it does not prove that the image is pullable by a Runner, matches its architecture, or runs successfully.

```bash
buz-manager pipeline plan pipeline.yml --step-index ./plugins
buz-manager pipeline plan pipeline.yml \
  --step-index ./plugins \
  --reviews ./reviews.json \
  --require-reviewed
buz-manager pipeline plan pipeline.yml --step-index ./plugins --offline
```

The review options are optional; strict review mode fails if any selected template hash is absent from the supplied manifest. Planning does not verify that referenced Manager Secrets or Variables exist or that a Runner is available. Use a disposable test pipeline and a real test Runner for runtime verification.

---

## Template Variables

All variables are injected as **environment variables** before each step runs. Commands reference them with standard shell `$VAR` syntax:

```yaml
steps:
  - name: Deploy
    run: |
      echo "Deploying $APP_NAME version $INPUT_VERSION"
      curl -X POST "$SLACK_WEBHOOK_URL"

  - name: Push image
    uses: buz/docker-login@1.1.0
    with:
      registry: crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com
      username: ${secrets.REGISTRY_USERNAME}
      password: ${secrets.REGISTRY_PASSWORD}
    run: |
      docker push crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/ns/app:$INPUT_VERSION
```

| Reference | Source | Level |
|---|---|---|
| `$APP_NAME` | `env:` block | Pipeline |
| `$BUILD_CACHE_DIR` | `variables:` (platform) | Pipeline |
| `$SLACK_WEBHOOK_URL` | `env: SLACK_WEBHOOK_URL: ${secrets.SLACK_WEBHOOK_URL}` | Pipeline / Step |
| `$GIT_SSH_KEY` | `env: GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}` | Step |
| `$INPUT_VERSION` | `on.manual.inputs:` | Trigger |

> Registry login/push goes through the `buz/docker-login` plugin template (login → push → logout), not a step field.
> **Note:** Parameter names from `on.manual.inputs` are **automatically uppercased** and prefixed with `INPUT_`. A parameter named `version` becomes `$INPUT_VERSION` inside commands.

---

## Steps

Each step is an atomic unit of work executed **sequentially** by the runner. Steps appear one after another in order.

### Step field index

Every registered key on `StepYaml` (`crates/shared/src/pipeline_yaml.rs`). Unknown keys are rejected.

| Field | Required | Engines | Summary |
|-------|:--------:|---------|---------|
| [`name`](#name-1) | ✓ | all | Display name |
| [`run`](#run) | ✓ | all | Command / script |
| [`engine`](#engine) | | all | `shell` / `local` / `ssh` / `docker` (default: shell) |
| [`image`](#image) | if docker | docker | Container image |
| [`volumes`](#volumes) | | docker | Extra `host:container` binds |
| [`privileged`](#privileged) | | docker | `--privileged` (DinD opt-in) |
| [`config`](#config) | | shell / local / ssh | Nested OSS deliver / render block |
| [`host`](#host--host_labels-ssh) / [`host_labels`](#host--host_labels-ssh) | ≥1 for ssh | ssh | Named hosts ∪ label matches |
| [`hosts_strategy`](#hosts_strategy-ssh) | | ssh | Multi-host fan-out: `chain` (default) \| `parallel` |
| [`work_dir`](#work_dir) / `dir` | | all | Working directory (`work_dir` aliases `dir`) |
| [`timeout`](#timeout) | | all | Per-step duration (`10m`, …) |
| [`retry`](#retry) | | all | ⚠️ Parsed but **not executed yet** — step runs once |
| [`continue-on-error`](#continue-on-error) | | all | Do not fail the job on step failure |
| [`env`](#env-1) | | all | Step env map (`KEY: ${secrets.X}`) |
| [`secrets`](#secrets-1) | | all | Optional step secret allowlist (no auto-inject) |

**Rejected legacy flat keys** (use nested [`config`](#config) instead): `config_source`, `config_path`, `config_dir`, `env_render`.

### Step Fields

### `name`

**Required.** Human-readable step name shown in Stage View and Build History.

| | |
|---|---|
| YAML | `name: Build and test` |
| Form | Step Name input |
| Required | Yes |

---

#### `run`

**Required** on every step (`engine` alone is not enough). The actual command(s) to execute. Can be a single line or a multi-line block (YAML `|`). Environment variables (from env, variables, secrets, inputs) are available as regular `$VAR` references. Typos like `ru:` are rejected — see [Field registration](#field-registration-required-before-use).

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

#### `engine`

Selects the execution engine. Defaults to `shell` (runner's own shell). When set, value **must** be one of `shell` / `local` / `ssh` / `docker` (other strings fail Validate).

| Value | Description |
|-------|-------------|
| _(omitted)_ | **Local** — runs on the runner host's shell |
| `shell` / `local` | Same as omitted — runner host shell |
| `ssh` | **SSH** — connects to remote host(s) via SSH (requires `host` and/or `host_labels`) |
| `docker` | **Docker** — runs `run:` inside a container on the runner. Requires `image`. See [Docker Engine](/docs/01-architecture/execution/docker-engine/). |

On a `uses:` step, `engine:` overrides the template's default engine — allowed only within the engines the template declares via `engines:` (checked at save time, "解析即锁定"). Note that with `ssh` the step's `work_dir`, `config.render` outputs and `run:` all execute on the target host, not the Runner.

| | |
|---|---|
| YAML | `engine: ssh` |
| Form | Engine dropdown (Runner Shell / SSH / Docker) |

> **Docker** is supported (MVP). See [Docker Engine](/docs/01-architecture/execution/docker-engine/). Enable **Docker** on the Runner in Manager UI.

---

#### `image`

**Docker only.** Container image reference used when `engine: docker`. Required for that engine; ignored otherwise.

```yaml
engine: docker
image: rust:1.85-bookworm
run: cargo test
```

| | |
|---|---|
| YAML | `image: node:20` |
| Form | Image (shown when Engine is Docker) |
| See | [Docker Engine](/docs/01-architecture/execution/docker-engine/) |

---

#### `volumes`

**Docker only.** Extra host→container bind mounts beyond the default workspace mount (`/buz/workspace`). Each entry is `host_path:container_path`.

```yaml
engine: docker
image: docker:27-cli
volumes:
  - /var/run/docker.sock:/var/run/docker.sock
run: docker version
```

Invalid on non-docker steps. Mounting the Docker socket grants root-equivalent control of the host — only on disposable runner VMs. See [Docker Engine](/docs/01-architecture/execution/docker-engine/).

| | |
|---|---|
| YAML | `volumes: ["/host:/container"]` |
| Runtime | `docker run -v …` |

---

#### `privileged`

**Docker only.** When `true`, runs the container with `--privileged` (needed for true Docker-in-Docker / nested containers). Default `false`.

```yaml
engine: docker
image: docker:27-dind
privileged: true
run: dockerd-entrypoint.sh & sleep 3 && docker info
```

Invalid on non-docker steps. Privileged containers can escape to the host — use only on isolated runners.

| | |
|---|---|
| YAML | `privileged: true` |
| Runtime | `docker run --privileged` |

---

#### Registry login / push

Registry auth is no longer a step field (removed in the 2.x retirement) — use the `buz/docker-login` plugin template (login → push → logout). See [Docker Engine](/docs/01-architecture/execution/docker-engine/) and `example/pipelines/acr-personal-push.yml`.

---

#### `config`

Nested **config block** for OSS delivery and/or declarative render (shell / local / ssh;
render not allowed on docker). Prefer **two steps**: deliver, then render-only.

```yaml
# OSS deliver — unpack only (deprecated → buz/config-fetch)
config:
  source: OSS_CONFIG          # Secret kind = Aliyun OSS auth; deprecated → buz/config-fetch
  key_path: config_tpl/app/$tag
  dest: /opt/app              # → $BUZ_CONFIG_DIR

# Render-only — templates already at dest
config:
  dest: /opt/app
  render:
    - template: env.example
      output: .env
      mode: "0600"
      on_missing: fail
      backup_keep: 3     # optional — timestamped .env.bak.<ts> backups to keep (0 disables)
```

`template` / `output` are **relative to `dest`**. Same root + subdirs → one step;
unrelated roots → separate render-only steps (see
[Migrating from the kernel `config:` block](/docs/01-architecture/authoring/migration-config-block/)).

Render is a **full rebuild** (the template is the sole source of truth), so keys
removed from the template are dropped. `backup_keep` (default `3`) keeps the
newest timestamped backups of the prior output for rollback.

Flat `config_source` / `config_path` / `config_dir` / `env_render` are **rejected**.
Full rules, DB columns, and engines: [Migrating from the kernel `config:` block](/docs/01-architecture/authoring/migration-config-block/).
Render engine details: [Migrating from the kernel `config:` block](/docs/01-architecture/authoring/migration-config-block/).

| | |
|---|---|
| YAML | nested `config:` |
| Form | Config (mode: None / OSS deliver / Render only / Deliver+render) |
| Runtime | `$BUZ_CONFIG_DIR`, `$BUZ_ENV_CHANGED`, `$BUZ_ENV_CHANGED_KEYS` |

---

#### `host` / `host_labels` (SSH)

**SSH only.** Provide **at least one** of `host` or `host_labels` (validator: requires `host` **or** `host_labels`). Neither field alone is mandatory. When both are set they form a **union** (deduplicated).

```yaml
# By Infrastructure → Hosts name (single or many — step runs on each)
host: prod-server-01
host:
  - prod-server-01
  - prod-server-02

# By Host labels — any host tagged prod OR web
host_labels: [prod, web]

# Both — named hosts ∪ label matches
host: bastion-01
host_labels: [web]
```

In the form: comma-separated for multiple names/labels. Help text states “at least one required; combined as union.”

**Multi-host execution:** when the union resolves to **N > 1** hosts, the step fans out according to [`hosts_strategy`](#hosts_strategy-ssh) (default **`chain`**). Set `parallel` for concurrent deploy. Job **Cancel** aborts **all** in-flight hosts. See [SSH Multi-Host Execution](/docs/01-architecture/execution/ssh-multi-host/).

| | |
|---|---|
| YAML | `host` and/or `host_labels` |
| Form | SSH Host / Host Labels |

---

#### `hosts_strategy` (SSH)

**SSH only.** Controls multi-host fan-out for connect, config prepare, and `run`.

| Value | Behavior |
|-------|----------|
| `chain` (default) | One host after another (wait-all exit aggregation) |
| `parallel` | All resolved hosts run concurrently (`join_all`) |

Omitted or empty → `chain`. Legacy alias `serial` is accepted and stored as `chain`. Invalid on non-SSH steps. Single-host steps ignore the distinction.

```yaml
host_labels: [prod]
hosts_strategy: parallel   # opt-in concurrent fan-out
```

| | |
|---|---|
| YAML | `hosts_strategy: chain \| parallel` |
| Runtime | Runner `step.hosts_strategy`; DB column `tasks.hosts_strategy` |

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

**Parsed but not executed yet.** The field is accepted so existing pipelines keep validating, but the runner currently executes the step exactly once and YAML validation emits a warning when `retry > 0`. Automatic step retries are planned for a future release; until then, use the job-level **Retry** action in the UI to re-run a failed job.

| | |
|---|---|
| YAML | `retry: 2` |
| Behavior | Not executed yet — step runs once (validation warning emitted) |
| Default | 0 |

---

#### `continue-on-error`

If `true`, a failure in this step does **not** stop the pipeline — subsequent steps continue to execute. The step status is still marked as `failed` in the UI.

| | |
|---|---|
| YAML | `continue-on-error: true` |
| Form | "Continue on error" checkbox |

---

#### `env`

**Step-level** environment map. Values may use `${secrets.NAME}` / `${vars.NAME}`; Manager expands them at claim time onto the process environment. Prefer this over bare allowlists when you need `$KEY` in `run`.

```yaml
env:
  APP_ENV: production
  GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}
run: echo "$APP_ENV" && test -n "$GIT_SSH_KEY"
```

Pipeline-level [`env`](#env) applies to all steps; step `env` merges / overrides for that step. Do **not** put `${secrets.…}` inside `run` — expand via `env:` first. See [Secrets Access](/docs/01-architecture/platform/secrets-access/).

| | |
|---|---|
| YAML | `env: { KEY: value \| ${secrets.X} }` |
| Runtime | Process env for this step |

---

#### `secrets`

Optional **step-level** secret name allowlist. Declaring a name does **not** inject `$NAME` into the environment — map via [`env`](#env-1). Use to narrow or document which secrets this step may reference.

```yaml
secrets: [GIT_SSH_KEY]
env:
  GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}
```

Full rules: [pipeline `secrets`](#secrets) and [Secrets Access](/docs/01-architecture/platform/secrets-access/).

| | |
|---|---|
| YAML | `secrets: [NAME]` |
| Into process env | Only via `env:` / purpose slots |

---

## Execution Engines

| Value | Engine | Description |
|-------|--------|-------------|
| _(omitted)_ | `local` | Run on runner host's shell (default) |
| `ssh` | `ssh` | Execute on remote host(s) via SSH (russh). Requires `host` and/or `host_labels`. |
| `docker` | `docker` | Execute `run:` in a Docker container on the runner. Requires `image`. Gated by Runner UI `docker_enabled`. See [Docker Engine](/docs/01-architecture/execution/docker-engine/). |

`wasm` is reserved for a future engine — declaring `engine: wasm` fails validation, and the Runner policy flag is dormant.

### Docker step fields

When `engine: docker`, YAML requires `image`. Optional docker-only fields: [`volumes`](#volumes), [`privileged`](#privileged). Workspace is bind-mounted to `/buz/workspace`. Full rules: [Docker Engine](/docs/01-architecture/execution/docker-engine/).

Examples: `example/pipelines/docker-engine-smoke.yml`, `example/pipelines/acr-personal-pull.yml`, `example/pipelines/acr-personal-push.yml`.

> Shelling out to `docker build` / `docker push` on the host is still a **local** (or **ssh**) step — that is not the Docker engine.

---

## YAML → runtime projection

Authoring is YAML-only ([YAML-First](./yaml-first-authoring.md)). On Validate / Save / scan / Trigger, Manager projects the file into DB tables (`pipelines`, `tasks`, bindings) for dispatch. Definition UI re-reads the **file** for display.

| YAML | DB / runtime | Notes |
|---|---|---|
| `name`, `description`, `team` / `owner_group` | `pipelines.*` / `team_id` | Unknown team slug rejected |
| `on.*` | `trigger_type`, webhook/cron fields, `input_defs` | |
| `runner.labels` / `timeout` | `runner_labels`, `timeout_seconds` | Duration strings parsed |
| `concurrency` | concurrency_* columns | Object or legacy string |
| `env` / `variables` / `secrets` | env rows + `var_ids` / `pipeline_secrets` | Unknown names rejected at Validate |
| `notify` | job-end notify channels | Expressions resolved at send time |
| `steps[]` | `tasks` rows | Nested `config:` → config_* columns |
| `steps[].engine` | `exec_type` | `shell`/`local`/`ssh`/`docker` only |
| `steps[].image` / `volumes` / `privileged` | `tasks.image` / `volumes` / `privileged` | Docker engine |
| `steps[].host` / `host_labels` / `hosts_strategy` | `tasks.*` | SSH; strategy default `chain` |
| `steps[].env` / `secrets` | `step_env` / `step_secrets` | |
| `steps[].continue-on-error` | `fail_strategy` | |

New YAML keys must be [registered](#field-registration-required-before-use) before use.
