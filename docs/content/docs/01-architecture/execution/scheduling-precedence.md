---
title: Scheduling Precedence
weight: 60
description: "Which gate decides what, and in what order — pipeline concurrency, environment deployment concurrency, approval, Runner capacity, resource slots, and queue/approval timeouts."
---

Five independent gates can stop a run between "trigger" and "a process starts".
They live in different components and fire at different moments, so "why is my
job not running?" is answered by walking this order top to bottom.

Related: [Job Concurrency & Queue](/docs/01-architecture/authoring/job-concurrency-queue/)
(pipeline-level modes), [Sandbox Host Configuration](/docs/01-architecture/execution/sandbox-host-configuration/)
(resource profiles and slots), [Runner Policy](/docs/01-architecture/execution/runner-policy/).

## The chain

| # | Gate | Evaluated | Where | On block |
|---|------|-----------|-------|----------|
| 1 | Pipeline disabled | Trigger | `do_trigger` | 409, no job row |
| 2 | Resource-profile grant (team) | Trigger | `do_trigger` | 403/validation, no job row |
| 3 | Input validation | Trigger | `do_trigger` | 422 with per-field message |
| 4 | Environment required + declared | Trigger | `do_trigger` | 422 — cannot be omitted |
| 5 | Environment host allowlist | Trigger | `do_trigger` | 422 (static hosts only) |
| 6 | **Environment protection** | Trigger | `do_trigger` | `waiting_approval` (automated triggers rejected) |
| 7 | **Pipeline concurrency** | Trigger (and again on approve) | `resolve_concurrency_status_in_tx` | 409 / `queued` / `pending` |
| 8 | Queue timeout | Background, leader, 5 s | `expire_queue_timeouts` | `queued` → `cancelled` |
| 9 | Approval timeout | Background, leader, 5 s | `expire_approval_timeouts` | `waiting_approval` → `cancelled` (24 h default) |
| 10 | Queue dispatch | Background, leader, 5 s | `dispatch_queued_jobs` | oldest `queued` → `pending` when the pipeline is idle |
| 11 | Runner frozen / restart pending | Claim | `poll_once` | no claim |
| 12 | Runner capacity | Claim | `poll_once` | no claim (`max_concurrent_jobs`) |
| 13 | Runner team binding + environment deployment concurrency | Claim (SQL prefilter) | `poll_once` | job not a candidate for that Runner |
| 14 | Runner labels | Claim | `poll_once` | job skipped |
| 15 | Resource-profile grant recheck | Claim | `poll_once` | job **fails** (grant revoked while queued) |
| 16 | **Sandbox host slot budget** | Claim, inside `BEGIN IMMEDIATE` | `slot_scheduler` | `backfill` skips / `fifo` ends the round |
| 17 | Environment deployment concurrency recheck | Claim, same transaction | `poll_once` | job skipped this round (`cancel_in_progress` supersedes in-flight) |
| 18 | Step timeout / cancel | Runtime | Runner engines | step `timeout` / `cancelled` |

## The three rules that actually decide conflicts

**Approval outranks concurrency.** A protected environment puts the job in
`waiting_approval` *without* consulting the concurrency group. Approvals are
counted first; when the threshold is met, the pipeline's concurrency mode is
evaluated in the same transaction (`jobs::approve` →
`resolve_concurrency_status_in_tx`). So an approved run can land in `queued`,
not just `pending`. A job in `waiting_approval` is not holding a queue slot, and
`queue_timeout` does not tick until it actually enters `queued`.

**Pipeline concurrency runs at trigger/approve; environment concurrency runs at
claim.** They are not layered at the same point:

- Pipeline-level `concurrency:` (per **pipeline**, see the caveat below) decides
  whether a new job is `pending`, `queued`, or rejected — only one
  `pending`/`running` job per pipeline exists at a time (plus up to `max_queue`
  queued ones).
- The environment's `deployment_concurrency` (`off` / `queue` /
  `cancel_in_progress`) is evaluated when a Runner tries to claim a `pending`
  job, together with the slot budget, inside the claim transaction. It governs
  deployments **to the same environment**, across pipelines.

A job can therefore pass trigger-time pipeline concurrency and still wait at
claim time because another pipeline is deploying to the same environment.

**Wait-for-capacity vs never-eligible.** At claim, `slot_scheduler` returns one
of three decisions:

- `Dispatch` — claim it.
- `WaitForCapacity` — the job fits in principle but the host budget is full.
  `backfill` moves on to the next candidate; `fifo` claims nothing this round;
  starvation aging (`scheduler_starvation_seconds`, default 600 s) ends a
  `backfill` round once a job has waited past the threshold, so the next freed
  capacity goes to it.
- `NotEligible` — this Runner/host can never run it (host mode with a non-default
  profile, profile not offered, profile larger than the host budget, stale
  config). Always skipped, by every strategy.

Each of these writes `dispatch_reason` on the job, visible on Job Detail.

`claim_strategy` and starvation aging only change behavior when a job can wait
for **resource slots**. With only the default profile (one slot each) and
`max_resource_slots = 0` (unlimited), `backfill` and `fifo` are equivalent;
starvation aging never triggers.

## Caveat: `concurrency.group` is a per-pipeline label

The `group:` string is stored and shown, but all concurrency SQL filters on
`pipeline_id`, not on the group value. Two pipelines that declare the same group
name do **not** serialize against each other. Per-environment serialization is
the mechanism that spans pipelines. Do not rely on a shared group name as a
global lock.

## Worked examples

**`concurrency: queue` + untrusted environment `queue`.**
Trigger: pipeline has a running job → new job is `queued` (up to `max_queue`).
Queue dispatch promotes it to `pending` when the pipeline goes idle. Claim: if
another pipeline is currently deploying to the same environment, the job is
skipped until that deployment finishes. Its `queue_timeout` clock runs from the
moment it entered `queued`, not from `pending`.

**Protected environment + `discard-old`.** Trigger creates
`waiting_approval`; no queued jobs are cancelled yet. On approval the mode is
evaluated: older `queued` jobs are cancelled and the approved job becomes
`pending`. Reject / timeout / cancel takes the job to `cancelled` and writes the
approval outcome into the snapshot.

**Two Runners, one sandbox host, one `large` (4-slot) job.** The host budget is
shared: Runner A claims the `large` job (4 of N slots), Runner B sees
`WaitForCapacity` for another `large` job and either backfills smaller jobs
(`backfill`) or idles (`fifo`). With `backfill`, the waiting `large` job is
protected from indefinite overtaking by starvation aging.

## Timeouts

| Timeout | Clock starts | Default | Source |
|---------|--------------|---------|--------|
| `queue_timeout` | entering `queued` (`updated_at`) | 30 m | frozen in the job snapshot |
| Approval timeout | job creation | 24 h | `environments.protection.approval_timeout_seconds` |
| Step `timeout` | step start | none (3600 s cap applied at runtime) | step YAML |
| Pipeline `runner.timeout` | — | 3600 s | stored on the pipeline |

Both background expiries cancel the job with a snapshot annotation
(`concurrency_queue_timed_out` / `approval.outcome = "timeout"`) and no audit
row of their own — the snapshot is the trail.
