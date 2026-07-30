---
title: Runner Policy (UI-managed)
weight: 25
description: "Centralized runner strategy — labels, concurrency, engines, cleanup — configured in Manager UI"
---

# Runner Policy (UI-managed)

Runner **strategy** (what the agent is allowed to do and how) is configured in **Manager UI / DB**, not in `runner.toml`.  
Machine-local settings (how to reach Manager and where to write files) stay in `runner.toml`.

This matches the same pattern as `max_concurrent_jobs`: one place to control all runners without editing files on each host.

## What stays in `runner.toml`

| Setting | Section | Purpose |
|---------|---------|---------|
| `manager.url` | `[manager]` | Manager base URL |
| `manager.register_token` | `[manager]` | One-time / bootstrap registration token |
| `manager.tls_skip_verify` | `[manager]` | Dev-only TLS skip |
| `runner.name` | `[runner]` | Must match the Runner name created in UI |
| `runner.workspace_dir` | `[runner]` | Local workspace root (`{workspace_dir}/{job_id}`) |
| `runner.prefer_interfaces` | `[runner]` | Multi-NIC IP ordering for UI |
| `engine.ssh_known_hosts_path` | `[engine]` | Path to `known_hosts` when UI policy is `known_hosts` |

Example:

```toml
[manager]
url = "https://buz-api.example.com"
register_token = "your-register-token"

[runner]
name = "prod-runner-01"
workspace_dir = "/tmp/buz-runner"

[engine]
# ssh_known_hosts_path = "/home/runner/.ssh/known_hosts"
```

## What is managed in Manager UI

Create or edit a Runner under **Runners** in the web UI.

| Setting | Default | Notes |
|---------|---------|-------|
| `labels` | `[]` | Scheduling match for pipeline `runner.labels` (must have **all** required) |
| `max_concurrent_jobs` | `1` | Parallel job slots, range `1`–`32` |
| `clean_workspace_after_job` | `false` | Delete `{workspace}/{job_id}` after each job (host files only) |
| `shell_enabled` | `true` | Allow `shell` / `local` steps |
| `ssh_enabled` | `true` | Allow `ssh` steps |
| `docker_enabled` | `false` | Allow `uses: docker` steps (`docker run --rm` per step) |
| `wasm_enabled` | `false` | Allow WASM steps (reserved) |
| `ssh_host_key_policy` | `insecure` | `insecure` (accept any) or `known_hosts` |

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
4. On `GET /runners/poll`, Manager enforces `running < max_concurrent_jobs` and claims only jobs whose required labels are all present on the runner.

### Cleanup: workspace vs Docker containers

| What | How |
|------|-----|
| Job workspace files on the runner host | `clean_workspace_after_job` (UI checkbox) |
| Container from a `uses: docker` step | Always `docker run --rm` — removed when the step ends; **not** a Runner Policy toggle |

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
- [API Reference — Runners](/docs/01-architecture/api-reference/) — create / update / register / heartbeat payloads
- [Docker Engine](/docs/01-architecture/docker-engine/) — gated by UI `docker_enabled`
- [Job Concurrency Queue](/docs/01-architecture/job-concurrency-queue/) — pipeline-level queue / discard modes
- [Deployment](/docs/03-operations/deployment/) — install + smoke path
