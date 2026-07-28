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

**Not this engine:** a local step that shells out to `docker build` / `docker push` on the runner. That is already `uses` omitted (local) or SSH. Docker engine means **the step command runs inside the container**.

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
| 7 | **Gate**: `engine.docker_enabled` must be `true`; otherwise step fails with a clear error (do not fall back to local) |
| 8 | **Scheduling hint**: pipelines that need Docker should set `runner.labels` to include `docker` (convention; not enforced by Manager in MVP) |
| 9 | **Private images**: MVP = operator/`run` does `docker login` (secret) or image already present; no first-class `image_pull_secret` yet |
| 10 | **Security defaults**: no `privileged`, no host network, no arbitrary CLI options in MVP |

---

## YAML Shape (MVP)

```yaml
steps:
  - name: Test
    uses: docker
    image: node:20-bookworm          # required
    work_dir: ./frontend             # optional; host path relative to job workspace
    # maps into container at /buz/workspace (cwd)
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
| `work_dir` / `dir` | | Host-side working directory to mount (same meaning as local) |
| `timeout` / `retry` / `continue-on-error` | | Same semantics as other engines |
| `host` / `host_labels` | ✗ | SSH-only; invalid with `uses: docker` (validator should warn/error) |

### Explicitly deferred YAML

```yaml
# NOT in MVP — do not document as supported until designed
options: "--cpus 1"           # free-form docker flags
volumes: ["x:/data"]          # extra mounts beyond workspace
network: host
privileged: true
services: […]
pull: always
entrypoint: /bin/bash
```

---

## Runtime Behavior

### Preconditions

1. Runner config: `engine.docker_enabled = true`
2. `docker` CLI (or equivalent API) reachable on the runner host
3. Step payload includes non-empty `image`

Failure messages should tell the operator which precondition failed (disabled vs missing binary vs empty image).

### Execution sketch

1. Resolve host `work_dir` under the job workspace (create if needed, same as local).
2. Build `docker run` arguments roughly:
   - `--rm`
   - `-i` (attach stdout/stderr for streaming logs)
   - `-v <host_work_dir>:/buz/workspace`
   - `-w /buz/workspace`
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
| Workspace mount | Host `work_dir` → `/buz/workspace` |
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

### A. Schema & validation

| # | Criterion | How to verify |
|---|-----------|---------------|
| A1 | Step with `uses: docker` and non-empty `image` parses successfully | Unit / YAML parse test |
| A2 | `uses: docker` without `image` (or empty `image`) is rejected at validate/import time with a clear error | Unit test |
| A3 | `uses: docker` combined with `host` / `host_labels` is rejected (or hard warning that blocks save) | Unit test |
| A4 | `owner_group` / unrelated fields unchanged; local and SSH steps still validate as today | Existing pipeline YAML tests still green |

### B. Runner gate & errors

| # | Criterion | How to verify |
|---|-----------|---------------|
| B1 | `engine.docker_enabled = false` → docker step fails; does **not** run on host shell | Unit or integration; assert error text mentions disabled |
| B2 | `docker_enabled = true` but `docker` binary / daemon unavailable → step fails with actionable message | Manual or integration |
| B3 | Empty/missing `image` in dispatched payload → step fails before `docker run` | Unit test |

### C. Execution semantics

| # | Criterion | How to verify |
|---|-----------|---------------|
| C1 | `run:` executes **inside** the named image (e.g. `cat /etc/os-release` or `node -v` matches image, not host) | Manual e2e on a docker-capable runner |
| C2 | Each docker step uses a fresh ephemeral container (`--rm`); no leftover container for that step after success/fail | `docker ps -a` after job; e2e |
| C3 | Host `work_dir` is visible at `/buz/workspace` and is the container cwd; file written in container appears on host path | E2e: write file in container, read on host |
| C4 | Pipeline env, variables, secrets, and trigger inputs are available as env vars inside the container | E2e: `echo $VAR` / secret name present |
| C5 | Secret values are masked in streamed Job logs (same masker behavior as local) | E2e or unit with masker fixture |
| C6 | Container non-zero exit code → step `failed`; zero → `success` | E2e or integration |
| C7 | Step `timeout` kills/stops the container; step ends as failed/cancelled consistently with local/SSH | E2e with `sleep` + short timeout |
| C8 | Job cancel while docker step is running stops the container and does not leave it running | E2e |

### D. Observability & product surface

| # | Criterion | How to verify |
|---|-----------|---------------|
| D1 | Stdout/stderr from the container stream to Job Detail step logs in near real time | Manual e2e |
| D2 | Example pipeline (not `docker-deploy.yml`) demonstrates `uses: docker` + `image` + `team:` and is documented | File review + docs link |
| D3 | Form: Engine=Docker shows required Image; SSH host fields hidden/disabled | Manual UI |
| D4 | Import YAML → Form maps `image`; Quick YAML Reference / pipeline-format mark Docker as supported for MVP fields | Manual + docs review |
| D5 | `cargo test --all` and `frontend` build pass; new docker unit tests included | CI |

### E. Explicit non-acceptance (must still hold)

| # | Criterion |
|---|-----------|
| E1 | MVP does **not** require `services:`, free-form `options:`, `privileged`, or host networking |
| E2 | `docker-deploy.yml`-style host `docker build` / SSH `docker run` remains local/SSH and does not depend on `docker_enabled` |
| E3 | Disabling `docker_enabled` never silently falls back to local execution |

### Smoke script (manual)

On a runner with Docker and `docker_enabled = true`:

1. Scan/import a minimal pipeline: one docker step, public image (e.g. `alpine:3.20`), `run: echo hello && uname -a`, `team: default`, labels including `docker`.
2. Trigger → Job Detail shows streamed `hello` and Alpine/`Linux` identity from the container.
3. Toggle `docker_enabled = false`, re-run same pipeline → step fails with disabled message, no host-shell success.

---

## Open Items (implementation-time)

These do **not** block design confirmation:

- Exact shell wrapper (`sh -lc` vs `bash -lc`) and Windows runner stance (MVP = Linux runners with Docker)
- Whether Manager should refuse to dispatch docker steps to runners that did not advertise a `docker` capability (today labels are soft matching only)
- First-class registry auth (`image_pull_secret`) vs documented `docker login` in a prior local step
