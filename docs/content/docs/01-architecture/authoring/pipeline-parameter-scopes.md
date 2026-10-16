---
title: Pipeline Parameter Scopes
weight: 3
description: "Pipeline YAML parameters only — top level / step / config block / template / runtime env, with each key's scope stated explicitly. Config-file (TOML) parameters live in Operations."
aliases:
  - /docs/01-architecture/authoring/pipeline-parameter-scopes/
---

# Pipeline Parameter Scopes

This page covers **pipeline authoring** parameters only — everything written in pipeline YAML or injected at step runtime. Four nested scopes:

```text
1. Pipeline       top-level YAML                → effect: one pipeline
2. Step           steps[] of a pipeline         → effect: one step
3. config block   steps[].config                → effect: one step's app-config files
4. Runtime env    BUZ_* / INPUT_*               → effect: injected per step
```

Every key is stated as three facts: **where it is declared** → **what it takes effect on** → **status**.

> The configuration **files** (`manager.toml` / `runner.toml`) are a different scope family — see [Config File Scopes](/docs/03-operations/config-file-scopes/).

## 1. Pipeline — top-level YAML

Scope: **one pipeline, all its steps**.

| Key | Takes effect on | Status |
|---|---|---|
| `name` / `description` / `team` | Identity; `team` scopes secrets and hosts | current |
| `on.manual` | Trigger dialog → `INPUT_*` | current |
| `on.webhook` (`secret:`) | HMAC-verified webhook trigger | current |
| `on.git` / `on.cron` | VCS / schedule triggers | current |
| `secrets` | Team-secret allowlist for this pipeline | current |
| `notify` | Channels; default events `success`, `failure` | current |

## 2. Step — `steps[]`

Scope: **one step**. `engine:` selects the executor; engine-specific keys are rejected elsewhere.

### 2.1 All engines

| Key | Takes effect on | Status |
|---|---|---|
| `name` | Display / logs | current |
| `run` | The script | current |
| `dir` (alias `work_dir`) | Working directory: runner host (shell) / container (docker) / remote (ssh) | current |
| `env` | Step process env; values may use `${secrets.X}` / `${vars.X}` | current |
| `timeout` | This step only | current |
| `continue-on-error` | Whether failure stops the pipeline | current |
| `secrets` | Narrows the pipeline allowlist for this step | current |
| `uses` + `with` | Template reference → §2.4 | current |
| `config` | App-config files → §3 | current |
| `registry_secret` | Registry login | **deprecated → `buz/docker-login`; removal planned** |

### 2.2 `engine: docker` only

| Key | Takes effect on | Status |
|---|---|---|
| `image` | Container image | current |
| `volumes` / `privileged` | Mounts / DinD | current |

### 2.3 `engine: ssh` only

| Key | Takes effect on | Status |
|---|---|---|
| `host` | Manager-resolved target list | current |
| `host_labels` | Target hosts by label | current |
| `hosts_strategy` | `chain` (default) / `parallel` fan-out | current |

### 2.4 Template steps (`uses:`) — who owns which key

| Owner | Keys | Takes effect on |
|---|---|---|
| **Template spec** | `run`, `image`, typed `inputs`, default `engine` | every call site of that template version |
| **Call site `engine:` override** | template-declared `engines` list (e.g. `[shell, ssh]`); incompatible override fails at claim time | this step only |
| **Call site** (kept through expansion) | `name`, `env`, `timeout`, `continue-on-error`, `secrets`, `host*`, `dir`, `volumes`, `privileged`, `registry_secret` | this step only |
| **`with`** | → `INPUT_<UPPER_SNAKE>` step env; validated against declared inputs | this step only |

## 3. `config` block — one step's app-config files

Scope: **the run block of this step**. Files already under `dest` (placed by any prior step — `buz/config-fetch`, `git clone`, a build artifact) are rendered in place before `run`.

| Key | Declared in | Takes effect on | Notes |
|---|---|---|---|
| `config.dest` | step YAML → `config` | `$BUZ_CONFIG_DIR` for this step | workspace-relative |
| `config.render[].template` / `output` | step YAML → `config` | output defaults to `<template>.rendered` | refs resolve `${secrets.X}` / `${vars.X}`; rendered values force-redacted |
| `config.render[].mode` | step YAML → `config` | output file mode (`0600` default) | |
| `config.render[].on_missing` | step YAML → `config` | `fail` (default) / `warn` | |
| `config.render[].backup_keep` | step YAML → `config` | timestamped `.bak.<ts>` copies (default 3) | |
| `config.render[].on_change` / `services` | step YAML → `config` | **parsed, not enforced** — validation warning | known limitation |

Rules: valid on `shell` / `local` / `ssh` only (invalid on `engine: docker`); exactly one `dest` per `config` block.

## 4. Runtime environment — `BUZ_*` / `INPUT_*`

Scope: **injected per step**, read-only for the author. Declared by the Runner (never by the user).

| Var | Injected when | Visible in | Status |
|---|---|---|---|
| `BUZ_JOB_URL` + job metadata | every step | all engines; notify deep links | current |
| `BUZ_CONFIG_DIR` | `config` block present | the step's config root | current |
| `DOCKER_CONFIG` | Runner-injected (`per_job` isolation) | job-scoped Docker CLI config dir — do not set manually | current |
| `INPUT_*` | `with:` values and manual-trigger inputs | masked when the template input declares `masking: true` | current |
| `BUZ_CONFIG_URL` | OSS deliver only | — | **retires with OSS delivery (Phase 3)** |
| `BUZ_REGISTRY_HOST/USER/PASSWORD` | ssh steps with `registry_secret` | remote login env | **retires with registry retirement (Phase 3)** |

## A complete example

One pipeline touching every scope in this page:

```yaml
# ── Scope 1: pipeline top level ──────────────────────────────────────
name: app-deploy
description: Build, push, and deploy the app
team: default

on:
  manual:
    inputs:
      image_tag:
        description: Image tag to build
        required: true
        default: "1.0.0"
        type: string
        pattern: "^[A-Za-z0-9._-]+$"

secrets:                          # team-secret allowlist for this pipeline
  - REGISTRY_PASSWORD
  - CONFIG_BUNDLE_URL
  - FEISHU_WEBHOOK

notify:
  - type: feishu
    webhook: ${secrets.FEISHU_WEBHOOK}
    # on: [success, failure]      # default

steps:
  # ── Scope 2.4: template step — credentials via with, masked ────────
  - name: Registry login
    uses: buz/docker-login@1.0.0
    with:
      registry: registry.example.com
      username: deploy
      password: ${secrets.REGISTRY_PASSWORD}

  # ── Scope 2.1: shell step — reuses the login above via DOCKER_CONFIG
  - name: Build and push
    engine: shell
    dir: app                      # relative → inside the job workspace
    run: |
      docker build -t "registry.example.com/app:${INPUT_IMAGE_TAG}" .
      docker push "registry.example.com/app:${INPUT_IMAGE_TAG}"

  # ── Scope 2.4 + 3: delivery (template) then render (config block) ──
  - name: Fetch config bundle
    uses: buz/config-fetch@1.0.0
    with:
      url: ${secrets.CONFIG_BUNDLE_URL}   # presigned OSS / S3 / MinIO / R2 URL
      dest: config

  - name: Deploy
    engine: shell
    config:                       # Scope 3: render-only, files already at dest
      dest: config
      render:
        - template: app.env.example
          output: app.env
          mode: "0600"
          on_missing: fail
    run: |
      set -a
      . "$BUZ_CONFIG_DIR/app.env" # Scope 4: runtime env injected by the Runner
      set +a
      ./deploy.sh

  # ── Scope 2.3: ssh step — host resolved from Infrastructure → Hosts ─
  - name: Restart service on target
    engine: ssh
    host: web-01
    dir: /opt/app
    run: docker compose up -d
```

What crosses scopes here: `${secrets.*}` are pipeline-scope references resolved at claim time; `INPUT_IMAGE_TAG` is a manual-trigger input (scope 1) surfaced as step env (scope 4); the login step's credentials live in the Runner's job-scoped `DOCKER_CONFIG` (a Runner-machine setting — see [Config File Scopes](/docs/03-operations/config-file-scopes/)) and are consumed by the later shell step; `BUZ_CONFIG_DIR` (scope 4) points at the `config.dest` (scope 3) populated by the fetch step.

## Deprecations at a glance

| Item | Scope today | Replacement | Plan |
|---|---|---|---|
| Step `registry_secret` | step key | `buz/docker-login` (Runner host) / inline `engine: ssh` login step (target hosts) | warning → removal (major release) |
| `BUZ_REGISTRY_*` runtime env | ssh step env | — (follows `registry_secret`) | same |
| `BUZ_CONFIG_URL` | step env (OSS deliver) | — (delivery is a step: `buz/config-fetch`) | same |
