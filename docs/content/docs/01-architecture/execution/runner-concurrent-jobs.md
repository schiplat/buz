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

| Mode | Supported features | Unsupported boundaries | Security isolation level |
|------|-------------------|------------------------|--------------------------|
| `host` | Shell/local steps; SSH steps; per-step `engine: docker` when Docker policy and host socket/runtime allow it; host-side config preparation, and existing pipeline features. | No OS sandbox: jobs run as the Runner OS user. SSH executes on the configured remote host, outside any local Runner isolation. | **L0 — no per-job OS isolation.** Jobs on the same Runner share its OS identity and accessible files/process environment. Suitable only for workloads within one trust domain. |
| `container` | Local/shell steps in one persistent job-scoped container; shared job workspace and cross-step files/`BUZ_ENV`; streamed, Runner-masked logs; CPU/memory/PID limits and hardened container settings. Requires successful capability check. | SSH, per-step Docker engine, and Runner-host config preparation are rejected before any step runs. No host fallback. No per-step-added host paths, Docker socket, or privileged mode. | **L1 — container containment.** Non-root, read-only root, dropped capabilities, no-new-privileges and resource limits, but shares the host kernel and trusts the Docker daemon and privileged `buz-sandboxd`. Not a strong boundary against kernel/runtime/daemon compromise. |
| `sandbox` | Local/shell steps in one persistent job-scoped container dispatched through a userspace-kernel OCI runtime (e.g. gVisor `runsc`); shared job workspace, cross-step files/`BUZ_ENV`, streamed masked logs and configured resource limits. Requires Linux and a successful live runtime probe; no `/dev/kvm` needed. | Same unsupported features and fail-closed behavior as Container. The actual boundary depends on the externally installed userspace-kernel runtime and its gofer/filesystem mediation. | **L2 — userspace-kernel sandbox.** Job syscalls are implemented by an application kernel instead of the host kernel, removing direct host-kernel exposure without KVM; the trust base is the runtime's syscall-emulation correctness (a memory-safe implementation, itself sandboxed). A different risk shape than L3, not a strictly weaker one. |
| `vm` | Local/shell steps in one job-scoped OCI workload dispatched through the host's Firecracker-compatible runtime; shared job workspace, cross-step files/`BUZ_ENV`, streamed masked logs and configured resource limits. Requires Linux, `/dev/kvm`, and a successful live runtime probe. | Same unsupported features and fail-closed behavior as Container. Buz does not directly launch Firecracker/jailer, build/provision the guest kernel, or make SSH targets part of the VM. Requires an operator-installed runtime, guest image and KVM access for the runtime. | **L3 — runtime-provided VM boundary.** Intended to add a KVM-backed guest boundary with a real guest kernel; the actual boundary depends on the externally installed OCI runtime, KVM/kernel, daemon and supervisor. It does not protect against compromise of those host trust components. |

The support checks and policy controls are intentionally separate: a platform admin selects the job-wide mode in Runner Policy, while the Runner periodically tests every configured tier backend. New Runners start in Host mode. The Manager only permits a switch to a sandboxed tier when the Runner is online and the corresponding capability result is supported and fresh (five minutes); the selected backend is checked again when each job starts. A failed capability check blocks selection and is shown with its reason in Runner details. The Runner only reports which tiers the host supports — which tier becomes a Runner's default execution mode is always the operator's decision; the system never auto-selects, upgrades, or downgrades a tier.

For step-level behavior and its own trust boundary, see [Docker Engine](/docs/01-architecture/execution/docker-engine/), [SSH multi-host execution](/docs/01-architecture/execution/ssh-multi-host/), and [Config Block](/docs/01-architecture/authoring/config-block/). These are distinct from `execution_mode`: every sandboxed tier (`container`, `sandbox`, `vm`) rejects their incompatible paths rather than running those steps on the host. See also the public [Security Notes](https://github.com/schiplat/buz/blob/master/SECURITY.md) for the broader Runner trust model.

All sandbox modes currently fail closed if the supervisor/backend is unavailable and reject jobs containing SSH, per-step Docker-engine steps, or Runner-host config preparation. Sandboxed tiers never fall back to Host and are never auto-selected or auto-switched. Step output is streamed through the supervisor and masked by Runner before upload; cancellation tears down the active sandbox. Network egress remains available on the configured container network unless restricted externally. The sandbox daemon has Docker-daemon access and is itself a privileged trust component; protect its Unix socket/config and pin runtime images.

**Tier model:** execution modes are isolation-strength tiers — `host` (L0) < `container` (L1) < `sandbox` (L2, userspace-kernel runtime such as gVisor `runsc`, no KVM required) < `vm` (L3) — with a pluggable per-tier OCI-runtime backend. Design, cloud KVM availability matrix, and rollout history: [Sandbox Isolation Tiers](/docs/01-architecture/execution/sandbox-isolation-tiers/).

### Who decides what: the authoring boundary

Isolation tiers are platform policy, not pipeline syntax. `execution_mode` never appears in pipeline YAML: it is selected per Runner by a platform admin in Runner Policy, and a job inherits the mode of whichever Runner claims it. The pipeline states what its code **needs**; the platform decides **how** — and how safely — it runs.

| Declared by | Where | Field | Question answered |
|-------------|-------|-------|-------------------|
| Pipeline author | pipeline YAML | `runner.labels` | Which capabilities does my job need? (`linux-x64`, `gpu`, …) |
| Pipeline author | pipeline YAML | `runner.resources` | How much compute does my job need? |
| Platform admin | Runner Policy (UI) | `execution_mode` | How are jobs on this machine isolated? |
| Platform admin | Runner Policy (UI) | Runner `labels` | Which capabilities does this machine provide? |
| Platform admin | Runner Policy (UI) | Team binding, profile grants | Whose jobs may land here, and with which resource envelope? |

**Labels carry capability vocabulary, never mechanism vocabulary.** Name Runners by what they offer (`linux-x64`, `gpu`, `arm64`), never by how they are isolated (`sandbox`). An author asks "is there a `linux-x64` machine?" without knowing or caring that gVisor sits behind it. Two Runners can share the label `linux-x64` while one runs `host` mode and the other `sandbox` mode; which one serves a job is an ops decision invisible to the pipeline. Once labels are named after tiers, `execution_mode` has effectively leaked into the pipeline through the back door.

Governance requirements — "this team's jobs must never run below L2" — are likewise not a pipeline field. They belong to the same admin-managed layer as profile grants, as a team-level minimum-tier check at dispatch. This check is not implemented yet; labels and team binding are today's only dispatch gates.

**Planned: pipeline-selected rootfs.** Today a sandboxed job's filesystem is the tier image declared in the host document, so that image must carry every toolchain any pipeline on the host may use — a pattern that scales poorly as toolchains accumulate. The planned direction: let the pipeline declare its own rootfs (e.g. `runner.image: node:20`) and keep the tier image as the fallback for pipelines that do not. The tier continues to own the isolation boundary; the image owns only the filesystem. Toolchain choice becomes an authoring concern; isolation stays a platform concern. The image-pull constraint for sandboxed tiers applies unchanged: pipeline-supplied images must be publicly pullable or pre-pulled on the host.

The platform-side view of the same model — tiers, profiles, and what the host actually offers — is in [Sandbox Host Configuration — How tiers, profiles, and host offers interact](/docs/01-architecture/execution/sandbox-host-configuration/#how-tiers-profiles-and-host-offers-interact).

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
