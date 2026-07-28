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
| C3 | Host `work_dir` visible at `/buz/workspace`; file written in container appears on host | `docker_e2e_workspace_mount_and_cwd` | ✅ |
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
