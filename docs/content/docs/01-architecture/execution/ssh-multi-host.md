---
title: SSH Multi-Host Execution
weight: 7
description: "One SSH step, many hosts — chain default / parallel opt-in, cancel-all, log banners"
aliases:
  - /docs/01-architecture/ssh-multi-host/
---

## Status

**Implemented** (Runner + YAML): multi-host SSH steps fan out per `hosts_strategy`
(default **`chain`** — one host after another). Opt in with
`hosts_strategy: parallel` for concurrent connect / config prepare / `run`.
Wait-all exit aggregation; job **Cancel** aborts all in-flight hosts; job-log
`==> SSH [user@host:port]` + `workspace` banner after login.

| Area | Behavior |
|------|----------|
| Connect / config prepare / `run` across N hosts | **`chain` (default)** or **`parallel`** |
| Step exit code with N hosts | **Any non-zero / error → step failed** |
| UI / API **Cancel** during multi-host work | Drops the active fan-out future → **all hosts** |
| Job log: target + remote workspace | `==> SSH […]` + `workspace` (`pwd`) |

Related: [Pipeline Format — `host` / `host_labels` / `hosts_strategy`](/docs/01-architecture/authoring/pipeline-format/#host--host_labels-ssh), [Config Block — SSH step session](/docs/01-architecture/authoring/config-block/), [Docker Engine cancel](/docs/01-architecture/execution/docker-engine/) (same job-cancel → runner kill path).

---

## Intent

A single pipeline step with `uses: ssh` may bind **multiple** Infrastructure Hosts via `host` and/or `host_labels` (union, deduplicated).

- **One host** — unchanged.
- **N hosts (N > 1)** — fan-out on the **same Runner**, same step:
  - `hosts_strategy: chain` (**default**) — one host after another
  - `hosts_strategy: parallel` — concurrent connect / prepare / `run`

```yaml
- name: Deploy
  uses: ssh
  host_labels: [prod]
  hosts_strategy: parallel   # omit for chain (default)
  run: |
    docker compose up -d
```

**History:** multi-host execution was always one-after-another (`for` loops). Parallel fan-out, cancel-all, login banners, and `hosts_strategy` came later. Default stays **`chain`** so existing pipelines keep that behavior until they opt into `parallel`. Legacy YAML value `serial` is accepted and normalized to `chain`.

This is **not** a job matrix: one Job, one step, N SSH sessions. Job-level concurrency (`max_concurrent_jobs`, pipeline `concurrency.group`) is a separate concern.

---

## Cancel (must not regress)

When the operator clicks **Cancel** (or the job otherwise enters `cancelled` while the SSH step is running):

1. The Runner’s existing cancel watcher must fire for the step (same path as shell / docker).
2. **Every** in-flight per-host task must be aborted — connect retries, config upload/`cat`/render, and remote `run` channels — not only the “current” host.
3. Open SSH sessions should be dropped promptly so remote processes are not left orphaned longer than necessary (best-effort; same class of guarantee as aborting a local shell / docker kill).
4. Job log should make cancel visible (existing `==> CANCELLED` / ERROR patterns); per-host partial output may remain if a host already finished before cancel.

**Do not** implement fan-out without wiring cancel to **all** child tasks (`select!` drop of `join_all` / chain loop future). Forgetting this leaves remotes deploying after the UI shows cancelled.

---

## Failure and exit aggregation

| Rule | Behavior |
|------|----------|
| **wait-all** | Let all hosts finish (unless cancelled / step timeout) — applies to both `parallel` and `chain`. |
| Step success | **All** hosts exit 0. |
| Step failure | **Any** host non-zero, connect/prepare error, or exec error → step failed. |
| Job log | Include a short per-host summary (host label + exit / error). Prefer buffering each host’s stdout/stderr until that host finishes, then append under `--- {step} @ {host} ---` so streams do not interleave unreadably. |

Step-level `timeout` still bounds the whole step (wall clock across the fan-out).

---

## Sessions and config prepare

- **One TCP session per host** for the step (connect retries on transient reset/refused/timeout).
- Config prepare and `run` **reuse** that session (new exec channels); no reconnect-per-command.
- OSS bundle: download **once** on the Runner, then fan-out upload/render to each host (`chain` or `parallel` per strategy).
- Sessions are not shared across steps or jobs.

---

## Job log (safe, concise)

After successful auth (and similarly on connect failure with the attempted target), emit a banner per host, for example:

```text
==> SSH [user@203.0.113.10:22]
  workspace  /home/deploy
```

| May log | Must not log |
|---------|----------------|
| Username, hostname/IP, port | Private keys, passwords, passphrases |
| Remote workspace (`pwd` / agreed cwd) | Secret env values, registry passwords |
| Host display name if present | Signed OSS URLs (except Debug) |
| `hosts_strategy` in Verbose `STEP CONFIG` | Credential material |

Align tone with existing `==> LOGIN [registry…]` / `==> RUN [ssh]` markers. Verbose `STEP CONFIG` should reflect resolved `host` objects (`user@host:port`) and `hosts_strategy`.

**Note:** today SSH does not always `cd` to YAML `work_dir` the way shell/docker do; the banner’s `workspace` is the **remote actual** working directory after login unless/until SSH honors `work_dir`.

---

## Non-goals

| Out of scope | Notes |
|--------------|-------|
| Job / step matrix with different `run` per host | Use separate steps or a future matrix |
| Guaranteed remote process kill on cancel | Best-effort session drop; OS may keep children |
| Changing pipeline `concurrency` semantics | Unrelated to in-step host fan-out |

---

## Implementation checklist

1. ~~`chain` multi-host (default, one after another).~~
2. ~~Parallel opt-in via `hosts_strategy: parallel`.~~
3. ~~Aggregate exit codes: any failure fails the step.~~
4. ~~**Cancel → abort all host tasks**.~~
5. ~~SSH login banner: target + workspace; no secrets.~~
6. ~~Verbose step-config host listing from resolved `host` payloads.~~

Primary code: `crates/runner/src/engine/ssh.rs`, `crates/runner/src/client/poll.rs`, `crates/runner/src/client/config_source.rs`, `crates/shared/src/pipeline_yaml.rs`.
