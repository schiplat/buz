---
title: Docker Engine
weight: 6
description: "Step-level Docker execution on the runner — design target aligned with GitHub Actions job container semantics"
---

## Status

**Implemented (MVP).** Step-level `uses: docker` with required `image`, ephemeral `docker run --rm`, workspace mount at `/buz/workspace`, gated by `engine.docker_enabled`. See acceptance criteria below.

Related: [Pipeline Format](/docs/01-architecture/pipeline-format/) (`uses` / execution engines), [Configuration](/docs/03-operations/configuration/).

Reference model: [GitHub Actions job `container`](https://docs.github.com/en/actions/using-jobs/running-jobs-in-a-container) — semantics and UX, not the full Actions product surface.

---

## Goals

- Run a pipeline **step** inside a Docker container on the **runner host**
- Reuse the existing step pipeline: logs, timeout, retry, cancel, env / secrets injection, log masking
- Keep YAML close to current `uses: ssh` style (engine chosen per step)
- Fail closed when Docker is disabled or unavailable

---

## Non-Goals (MVP)

| Out of scope | Notes |
|--------------|-------|
| Job/pipeline-wide shared container | Prefer one `docker run --rm` per docker step |
| Service / sidecar containers | Actions `services:` — defer |
| Arbitrary `docker create` option strings | Actions `container.options` — security escape hatch; defer or whitelist later |
| Docker Action marketplace / `docker://` entrypoint-args model | Buz keeps a single `run:` command model |
| Remote Docker / Docker Context / Swarm / Compose orchestration | Use `uses: ssh` + host `docker …` (see `example/pipelines/docker-deploy.yml`) |
| Manager-side containers | Execution stays on Runner only |
| Wasm engine | Separate Phase 3 track |

**Not this engine:** a shell step that shells out to `docker build` / `docker push` on the runner. That is already `uses` omitted (shell) or SSH. Docker engine means **the step command runs inside the container**.

---

## Docker-in-Docker (DinD) & Building Container Images

When a pipeline needs to **build, push, or manage Docker images** inside a step (e.g. `docker build`, `docker push`, `docker compose`), the step needs access to a Docker daemon. This is the "Docker-in-Docker" (DinD) problem.

### How GitHub Actions Solves It

GitHub Actions uses **privileged-mode DinD**: each runner pod starts with a `docker:dind` sidecar container granted `--privileged`. This gives the sidecar the **same kernel capabilities as the host**, meaning:

- The inner Docker daemon can create containers, manage volumes, and use host networking.
- A compromised job can escape to the host via `nsenter` or kernel exploits.
- **Mitigation**: GitHub runs actions on **disposable, isolated VMs** — each job gets a fresh environment destroyed after completion.

The security model is: "the runner VM is sacrificial, never shared across teams or tenants."

### Three Approaches (Ranked by Security)

| Approach | How It Works | Security | Use Case |
|---|---|---|---|
| **1. Docker socket mount** | `-v /var/run/docker.sock:/var/run/docker.sock` | **🔴 Root-equivalent to host** — container controls host daemon | Fastest; trusted internal CI only |
| **2. Privileged DinD** | `docker run --privileged docker:dind` as sidecar | **🟡 Can escape host** via `nsenter`; limited by disposable runner | GitHub Actions style; requires VM-level isolation |
| **3. Daemonless builders** | Run `kaniko` / `buildah` in an unprivileged container | **🟢 No daemon access needed**; builds to registry | Production CI/CD; recommended for public repos |

### Recommended: Option 2 — Privileged DinD with Disposable Runners

This aligns with GitHub Actions' approach and is the pragmatic choice for most CI/CD pipelines.

**Prerequisites:**
1. The runner VM must have Docker installed on the host.
2. The runner VM must be **dedicated and disposable** (not shared with other workloads).
3. The runner config must enable DinD.

**Step 1: Enable DinD in runner config**

```toml
# runner.toml

[runner]
name = "ci-builder-01"
labels = ["linux-x64", "docker", "dind"]
workspace_dir = "/tmp/buz-runner"

[engine]
shell_enabled = true
docker_enabled = true
dind_enabled = true            # enables privileged docker:dind sidecar
```

**Step 2: Pipeline YAML with `image: docker:dind`**

```yaml
name: Build and Push
description: Build a Docker image and push to registry

on: manual

runner:
  labels: [linux-x64, docker, dind]

env:
  REGISTRY: registry.example.com
  IMAGE_NAME: my-app

steps:
  # Step 1: Build the image (runs inside docker:dind)
  - name: Build
    uses: docker
    image: docker:27-dind
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock   # share host daemon
    run: |
      docker build -t $REGISTRY/$IMAGE_NAME:$BUILD_NUMBER .
    timeout: 10m

  # Step 2: Push to registry
  - name: Push
    uses: docker
    image: docker:27-dind
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
    run: |
      docker push $REGISTRY/$IMAGE_NAME:$BUILD_NUMBER
    timeout: 5m
```

**Step 3 (Alternative): True nested DinD without socket sharing**

```yaml
  - name: Build
    uses: docker
    image: docker:27-dind
    privileged: true                                   # future: --privileged flag
    run: |
      dockerd &                                        # start inner daemon
      sleep 3
      docker build -t $REGISTRY/$IMAGE_NAME:$BUILD_NUMBER .
    timeout: 10m
```

### Option 3 (Advanced): Daemonless Builds with Kaniko

For production environments where runner VM isolation isn't guaranteed:

```yaml
  - name: Build
    uses: docker
    image: gcr.io/kaniko-project/executor:latest
    volumes:
      - /path/to/docker-config.json:/kaniko/.docker/config.json:ro
    run: |
      /kaniko/executor \
        --context $CI_PROJECT_DIR \
        --destination $REGISTRY/$IMAGE_NAME:$BUILD_NUMBER
    timeout: 10m
```

No Docker daemon, no privileged mode, no socket sharing. Builds go directly to the registry.

### Security Summary

```
┌────────────────────────────────────────────────────┐
│ Socket mount  →  Host daemon exposed               │  "just for CI" is NOT safe
│ Privileged    →  nsenter escape possible            │  Safe ONLY with disposable runners
│ Daemonless    →  No daemon, no escape vector        │  Recommended for production
└────────────────────────────────────────────────────┘
```

> **Warning**: Mounting `/var/run/docker.sock` into a container gives that container **root-equivalent control of the host**. Any `RUN` step in a Dockerfile, any `npm install` script, or any build tool with shell access can create privileged containers on the host machine. Only use socket mounting on **dedicated, isolated runner VMs** where a compromise is acceptable.

### Current MVP Status

As of the Docker engine MVP:

- ✅ `volumes` support allows mounting `/var/run/docker.sock` — **user opt-in**.
- ◻ `privileged: true` on steps is deferred (not yet implemented).
- ◻ `dind_enabled` config gate is deferred.
- ◻ Kaniko example is documented but not yet tested E2E.

The path to full DinD support requires the `privileged` flag on step declarations (`crates/shared/src/pipeline_yaml.rs`) and the `--privileged` argument in `crates/runner/src/engine/docker.rs`.

---

## Mental Model

```
local   → runner host shell executes `run`
ssh     → remote host (Hosts registry) executes `run`
docker  → `docker run … <image>` on the runner; container executes `run`
```

```
┌─────────────┐     claim job      ┌──────────────┐
│   Manager   │ ─────────────────► │    Runner    │
└─────────────┘                    │  (host dockerd)│
                                   │       │        │
                                   │  docker run    │
                                   │       ▼        │
                                   │  ┌──────────┐  │
                                   │  │  image   │  │  ← step `run` here
                                   │  └──────────┘  │
                                   └──────────────┘
```

---

## Confirmed Product Rules

| # | Decision |
|---|----------|
| 1 | **Granularity**: step-level `uses: docker` (not pipeline-level `container:` in MVP) |
| 2 | **Lifecycle**: one ephemeral container per docker step (`--rm`); no reuse across steps |
| 3 | **Required field**: `image` when `uses: docker` |
| 4 | **Command model**: same as local/SSH — multi-line `run:` executed via container shell (`sh -lc` unless overridden later) |
| 5 | **Workspace**: bind-mount runner step work directory into a fixed container path (default `/buz/workspace`); container cwd = that path |
| 6 | **Env / secrets / inputs**: inject as container env (`-e`), same resolution order as local; masking unchanged |
| 7 | **Gate**: `engine.docker_enabled` must be `true`; otherwise step fails with a clear error (do not fall back to shell) |
| 8 | **Scheduling**: pipelines that need Docker should set `runner.labels` to include `docker`; Manager dispatches only to runners that have **all** required labels |
| 9 | **Private images**: MVP = operator/`run` does `docker login` (secret) or image already present; no first-class `image_pull_secret` yet |
| 10 | **Security defaults**: no `privileged`, no host network, no arbitrary CLI options in MVP |

---

## YAML Shape (MVP)

```yaml
steps:
  - name: Test
    uses: docker
    image: node:20-bookworm          # required
    work_dir: ./frontend             # optional; container-side subdirectory under /buz/workspace
    # runner workspace is always mounted at /buz/workspace
    timeout: 15m
    run: |
      npm ci
      npm test
```

| Field | Required | Description |
|-------|:--------:|-------------|
| `uses: docker` | ✓ | Selects Docker engine |
| `image` | ✓ | Image reference (`name:tag` or digest). Pulled if missing (`docker pull` / run pull behavior TBD in impl) |
| `run` | ✓ | Command(s) inside the container |
| `work_dir` / `dir` | | Container-side subdirectory under `/buz/workspace` (runner workspace always mounted at root) |
| `timeout` / `retry` / `continue-on-error` | | Same semantics as other engines |
| `host` / `host_labels` | ✗ | SSH-only; invalid with `uses: docker` (validator should warn/error) |

### Explicitly deferred YAML

```yaml
# NOT in MVP — do not document as supported until designed
options: "--cpus 1"           # free-form docker flags
network: host
privileged: true
services: […]
pull: always
entrypoint: /bin/bash

# Supported since MVP ✅
volumes: ["/host/path:/container/path"]
```

---

## Runtime Behavior

### Preconditions

1. Runner config: `engine.docker_enabled = true`
2. `docker` CLI (or equivalent API) reachable on the runner host
3. Step payload includes non-empty `image`

Failure messages should tell the operator which precondition failed (disabled vs missing binary vs empty image).

### Execution sketch

1. Runner's workspace_dir (from `runner.toml`) is bind-mounted as the container workspace.
2. Build `docker run` arguments roughly:
   - `--rm`
   - `-i` (attach stdout/stderr for streaming logs)
   - `-v <runner.workspace_dir>:/buz/workspace`
   - `-w /buz/workspace[/<work_dir>]` (defaults to workspace root)
   - `-e KEY=VALUE` for injected env (including secrets — memory only on runner, never logged raw)
   - `<image>`
   - `sh -lc '<run script>'` (exact argv encoding left to implementation)
3. Stream container stdout/stderr through the existing log path (masker applied).
4. Exit code of the container is the step exit code.
5. On step timeout or job cancel: stop/kill the container, then treat like other engines.

### Pull policy (MVP recommendation)

- Prefer `docker run` default pull-if-missing, or an explicit `docker pull` before run when the image is absent.
- Document that air-gapped runners must pre-load images.
- No `pull: always` knob until needed.

---

## Comparison with GitHub Actions

| Actions | Buz MVP |
|---------|---------|
| Job-level `container.image` | Step-level `uses: docker` + `image` |
| All job steps share one container | Fresh container per docker step |
| `container.env` / job env | Pipeline + step env / variables / secrets → `-e` |
| Workspace mount | Runner `workspace_dir` → `/buz/workspace` |
| `container.options` | Deferred |
| `services:` | Deferred |
| `uses: docker://image` + entrypoint/args | Not used; keep `run:` |
| Host `run: docker build` | Remains **local** / **ssh**, not this engine |

---

## Config

```toml
[engine]
docker_enabled = false   # set true on runners that should accept uses: docker
```

Optional later (not required for MVP design sign-off):

- `docker_binary` / socket path overrides
- default workspace container path override

---

## Form / UI (when implementing)

| YAML | Form |
|------|------|
| `uses: docker` | Engine = Docker |
| `image` | Image (required when Engine is Docker) |
| `host` / `host_labels` | Hidden or disabled for Docker engine |

Import-from-YAML should map `image` like other step fields once the parser gains the field.

---

## Implementation Checklist (when coding starts)

1. `PipelineYaml` / step schema: add `image`; validate `uses: docker` ⇒ `image` present; reject `host` combo
2. Protocol / job step payload: carry `image` to runner
3. `crates/runner/src/engine/docker.rs` + poll dispatch when `docker_enabled`
4. Tests: argv construction, disabled gate, missing image, comment/fixture without needing a real daemon where possible
5. Example pipeline using `uses: docker` (distinct from `docker-deploy.yml`)
6. Update [pipeline-format](/docs/01-architecture/pipeline-format/) status from “stub” to “supported”

---

## Acceptance Criteria

MVP is **done** only when all items below pass. Criteria are testable (automated where noted, otherwise manual e2e).

**Status legend:** ✅ verified in repo · ⚠ manual / optional product path · ◻ open

### A. Schema & validation

| # | Criterion | How to verify | Status |
|---|-----------|---------------|:------:|
| A1 | Step with `uses: docker` and non-empty `image` parses successfully | `crates/shared/tests/pipeline_yaml_test.rs` | ✅ |
| A2 | `uses: docker` without `image` (or empty `image`) is rejected at validate/import time with a clear error | same | ✅ |
| A3 | `uses: docker` combined with `host` / `host_labels` is rejected (or hard warning that blocks save) | same | ✅ |
| A4 | `owner_group` / unrelated fields unchanged; local and SSH steps still validate as today | Existing pipeline YAML tests still green | ✅ |

### B. Runner gate & errors

| # | Criterion | How to verify | Status |
|---|-----------|---------------|:------:|
| B1 | `engine.docker_enabled = false` → docker step fails; does **not** run on host shell | `docker_tests::preflight_rejects_disabled` | ✅ |
| B2 | `docker_enabled = true` but `docker` binary / daemon unavailable → step fails with actionable message | Spawn error text in `execute_streaming`; covered when daemon down | ✅ |
| B3 | Empty/missing `image` in dispatched payload → step fails before `docker run` | `docker_tests::preflight_rejects_empty_image` | ✅ |

### C. Execution semantics

| # | Criterion | How to verify | Status |
|---|-----------|---------------|:------:|
| C1 | `run:` executes **inside** the named image (e.g. Alpine identity, not host) | `docker_e2e_runs_inside_alpine_not_host` + smoke YAML | ✅ |
| C2 | Each docker step uses a fresh ephemeral container (`--rm`); no leftover for that step | argv unit + `docker_e2e_rm_does_not_grow_container_list` | ✅ |
| C3 | Runner workspace at `/buz/workspace`; file written in container appears on host | `docker_e2e_workspace_mount_and_cwd` | ✅ |
| C4 | Pipeline env / variables / secrets / inputs available as env inside the container | `docker_e2e_env_injection` (+ `INPUT_*`) | ✅ |
| C5 | Secret values are masked in streamed Job logs (same masker as local) | `masker` unit tests | ✅ |
| C6 | Container non-zero exit → step failed; zero → success | `docker_e2e_nonzero_exit_fails` | ✅ |
| C7 | Step `timeout` kills/stops the container | Runner timeout path + `docker_e2e_kill_stops_container` (kill child) | ✅ |
| C8 | Job cancel while docker step is running stops the container | Same kill path as C7 (Manager cancel → runner kills child) | ✅ |

### D. Observability & product surface

| # | Criterion | How to verify | Status |
|---|-----------|---------------|:------:|
| D1 | Stdout/stderr stream to Job Detail step logs | Shared streaming path; confirm with smoke job | ⚠ |
| D2 | Example pipeline demonstrates `uses: docker` + `image` + `team:` | `example/pipelines/docker-engine-smoke.yml` | ✅ |
| D3 | Form: Engine=Docker shows required Image; SSH host fields hidden | Pipeline create/edit UI (manually confirmed) | ✅ |
| D4 | Import YAML → Form maps `image`; pipeline-format marks Docker supported | Import path + docs | ✅ |
| D5 | `cargo test --all` and `frontend` build pass; docker unit tests included | CI + `make test-docker-e2e` for ignored daemon tests | ✅ |

### E. Explicit non-acceptance (must still hold)

| # | Criterion | Status |
|---|-----------|:------:|
| E1 | MVP does **not** require `services:`, free-form `options:`, `privileged`, or host networking | ✅ |
| E2 | `docker-deploy.yml`-style host `docker build` / SSH `docker run` remains local/SSH | ✅ |
| E3 | Disabling `docker_enabled` never silently falls back to local execution | ✅ |

### Smoke / e2e commands

**Automated (runner engine, real daemon):**

```bash
make test-docker-e2e
# equivalent:
BUZ_DOCKER_E2E=1 cargo test -p runner docker_e2e -- --ignored --nocapture
```

Requires Docker CLI + daemon. Tests are `#[ignore]` so default `cargo test --all` stays green without Docker.

**Manual product smoke** (Manager + Runner):

1. Runner: `engine.docker_enabled = true`, labels include `docker`.
2. Scan/import `example/pipelines/docker-engine-smoke.yml`.
3. Trigger → Job Detail shows streamed `hello` and Alpine identity.
4. Toggle `docker_enabled = false`, re-run → step fails with disabled message, no host-shell success.

---

## Open Items (non-blocking)

These do **not** block MVP acceptance:

- Exact shell wrapper (`sh -lc` vs `bash -lc`) and Windows runner stance (MVP = Linux runners with Docker)
- First-class registry auth (`image_pull_secret`) vs documented `docker login` in a prior local step
- Full Manager↔Runner cancel e2e in CI (covered by engine kill + existing cancel wiring)
