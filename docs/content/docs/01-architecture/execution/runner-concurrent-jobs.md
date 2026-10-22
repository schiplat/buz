---
title: Runner Policy (UI-managed)
weight: 25
description: "Centralized runner strategy — labels, concurrency, engines, cleanup — configured in Manager UI"
aliases:
  - /docs/01-architecture/runner-concurrent-jobs/
---

Runner **strategy** (what the agent is allowed to do and how) is configured in **Manager UI / DB**, not in `runner.toml`.  
Machine-local settings (how to reach Manager and where to write files) stay in `runner.toml`.

This matches the same pattern as `max_concurrent_jobs`: one place to control all runners without editing files on each host.

## What stays in `runner.toml`

| Setting | Section | Purpose |
|---------|---------|---------|
| `manager_url` | `[runner]` | Manager base URL |
| `register_token` | `[runner]` | One-time / bootstrap registration token |
| `tls_skip_verify` | `[runner]` | Dev-only TLS skip |
| `name` | `[runner]` | Must match the Runner name created in UI |
| `workspace_dir` | `[runner]` | Local workspace root (`{workspace_dir}/{job_id}`) |
| `prefer_interfaces` | `[runner]` | Multi-NIC IP ordering for UI |
| `ssh_known_hosts_path` | `[engine]` | Path to `known_hosts` when UI policy is `known_hosts` |
| `level` / `file` | `[logging]` | Log verbosity and optional file path |

Example:

```toml
[runner]
name = "prod-runner-01"
manager_url = "https://buz-api.example.com"
register_token = "your-register-token"
workspace_dir = "/tmp/buz-runner"

[engine]
# ssh_known_hosts_path = "/home/runner/.ssh/known_hosts"

[logging]
level = "info"
```

## What is managed in Manager UI

Create or edit a Runner under **Runners** in the web UI.

| Setting | Default | Notes |
|---------|---------|-------|
| `team_id` | *(required at create)* | Dispatch scope: a specific team, or **Shared** (`NULL`). Bound Runners only receive jobs whose trigger-time team matches; `NULL` serves every team. Existing runners keep `NULL`/Shared after upgrade |
| `execution_mode` | `host` | Job-wide executor: direct Host, persistent job Container, or a VM-backed OCI runtime configured on the host. Per-step `engine` remains a separate choice and is checked for compatibility |
| `labels` | `[]` | Scheduling match for pipeline `runner.labels` (must have **all** required) |
| `max_concurrent_jobs` | `1` | Parallel job slots, range `1`–`32` |
| `clean_workspace_after_job` | `false` | Delete `{workspace}/{job_id}` after each job (host files only) |
| `shell_enabled` | `true` | Allow `shell` / `local` steps |
| `ssh_enabled` | `true` | Allow `ssh` steps |
| `docker_enabled` | `false` | Allow `engine: docker` steps (`docker run --rm` per step) |
| `wasm_enabled` | `false` | Allow WASM steps (reserved) |
| `ssh_host_key_policy` | `known_hosts` | `insecure` (accept any) or `known_hosts` — **secure default since v1.6**; existing rows keep their stored value |

## How policy is delivered

```text
Admin (UI) ──PUT /runners/:id──► Manager DB (labels + policy columns)
                                      │
Runner process                        │
  register  ◄── policy snapshot ──────┤  (does NOT overwrite labels)
  heartbeat ◄── policy snapshot ──────┘  (~every 30s)
       │
       ▼
  SharedPolicy (in-memory)
       │
       ├── poll capacity (max_concurrent_jobs)
       └── per-job Config overlay (engines, cleanup, host-key policy)
```

1. Admin sets **labels** and policy when creating a Runner, or saves them in the Runner detail drawer.
2. On `POST /runners/register` and `POST /runners/heartbeat`, Manager returns a `policy` snapshot for engines/concurrency. Labels stay in the DB and are used at poll time; the agent register payload **cannot** overwrite them.
3. The Runner applies the snapshot on the next poll / job without restart. A failed or successful job **never** stops a persistent runner process.
4. On `GET /runners/poll`, Manager enforces `running < max_concurrent_jobs` and claims only jobs whose required labels are all present on the runner — and whose frozen trigger-time team matches the runner's team binding (Shared/`NULL` matches all).

### Team binding is dispatch scoping, not a sandbox

Team binding and labels answer different questions: **labels** match a job's requested capabilities (`linux-x64`, `gpu`, …); **team binding** restricts which team's jobs the Manager may dispatch to this Runner. Neither isolates running jobs from one another — all jobs on a Runner execute as the Runner's OS user and can access that user's files and process environments. Put mutually untrusted teams on dedicated Runner hosts/users. Jobs freeze their team at trigger time; a pipeline cannot change `team` while it has active (pending/queued/waiting-approval/running) jobs. Jobs created before team freezing carry no frozen team: bound Runners skip them, Shared Runners handle them under the pipeline's current team.

### Job-wide execution modes

`execution_mode` is selected by a platform admin in Runner Policy; it does **not** rewrite a pipeline's per-step `engine` and a job never falls back to Host when its selected backend is unavailable.

| Mode | Job behavior | Host requirements / limits |
|------|--------------|----------------------------|
| `host` | Current behavior: local/shell steps run as the Runner OS user; Docker engine remains one `docker run --rm` per Docker step; SSH runs on the configured remote host. | Default for new and existing Runners; no sandbox. |
| `container` | All local/shell steps in one job execute in one persistent job container with the shared job workspace. | Requires the host-side `buz-sandboxd`, Docker CLI/daemon, a pinned `sandbox.container_image`, and matching shared workspace volume configuration when sandboxd is containerized. Container shares the host kernel; it is containment, not VM isolation. |
| `vm` | Local/shell steps execute in one job-scoped OCI container dispatched through the host's configured Firecracker-compatible runtime. | Requires KVM, a host Docker-compatible runtime registered with the daemon, `sandbox.firecracker_guest_image`, `sandbox.firecracker_runtime`, and `buz-sandboxd`. Buz currently delegates OCI container creation to that runtime; it does not directly launch Firecracker/jailer or provision a guest kernel/image. |

Container/VM modes fail closed if the supervisor/runtime is missing and currently reject jobs containing SSH or per-step Docker engine steps, registry-auth handoff, or Runner-host config preparation. These paths are not silently executed on the host. Step output is returned through the supervisor and masked by Runner before upload; cancellation tears down the job sandbox. Network egress is still available on the configured container network unless the operator restricts it externally. The sandbox daemon has Docker-daemon access and is itself a privileged trust component; protect its Unix socket/config and pin runtime images.

Runner capability checks create, start, execute a shell/timeout check in, and remove a short-lived pinned-image workload on startup and every five minutes. VM additionally verifies Linux and `/dev/kvm` and exercises the configured external Firecracker-compatible OCI runtime. Runner details display each backend's latest result and reason. Manager permits switching to Container/VM only for an online Runner with a successful check no older than five minutes; execution performs its normal backend create checks again.

A Container shares the host kernel and must not be represented as a strong boundary against a malicious host/container-runtime exploit. Use separate Runner host/OS identities or disposable VM-backed jobs for mutually untrusted workloads; SSH targets always require their own isolation controls.

### Cleanup: workspace vs Docker containers

| What | How |
|------|-----|
| Job workspace files on the runner host | `clean_workspace_after_job` (UI checkbox) |
| Container from a `engine: docker` step | Always `docker run --rm` — removed when the step ends; **not** a Runner Policy toggle |

Label edits take effect on the **next poll**. Engine / concurrency edits typically within **one heartbeat (~30s)**.

## Concurrent jobs

- Default `1` = serial (backward-compatible).
- Each claimed job uses an isolated workspace: `{workspace_dir}/{job_id}`.
- Independent of **pipeline** concurrency (`concurrency.group` / queue modes), which limits how many jobs a *pipeline* may have active. Runner concurrency limits how many jobs a *machine* may run at once.

## Engines and SSH host keys

- Disabled engines fail the step with a clear error (no silent fallback to shell).
- For production SSH, set UI policy to `known_hosts` and ensure the local file exists (`~/.ssh/known_hosts` or `engine.ssh_known_hosts_path`).

## Upgrade notes

Existing `runner.toml` keys such as `runner.labels`, `engine.docker_enabled`, `clean_workspace_after_job`, and `ssh_host_key_policy` still deserialize for compatibility, but labels are **ignored on register**, and engine/cleanup keys are **overwritten** by the Manager policy after register/heartbeat.

After upgrading:

1. Restart Manager (migrations add policy columns with safe defaults).
2. Open **Runners** and set **labels** / engines / cleanup as needed (labels previously only in TOML will no longer be applied on register).
3. Restart or wait for Runner heartbeat to pick up the snapshot.

## Database

Policy columns live on `runners` (see [Database Schema](/docs/02-development/database-schema/)):

`max_concurrent_jobs`, `clean_workspace_after_job`, `shell_enabled`, `ssh_enabled`, `docker_enabled`, `wasm_enabled`, `ssh_host_key_policy`.

## Related

- [Configuration Reference](/docs/03-operations/configuration/) — machine-local `runner.toml` keys
- [API Reference — Runners](/docs/01-architecture/platform/api-reference/) — create / update / register / heartbeat payloads
- [Docker Engine](/docs/01-architecture/execution/docker-engine/) — gated by UI `docker_enabled`
- [Job Concurrency Queue](/docs/01-architecture/authoring/job-concurrency-queue/) — pipeline-level queue / discard modes
- [Deployment](/docs/03-operations/deployment/) — install + smoke path
