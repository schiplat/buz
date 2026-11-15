---
title: Transaction Boundaries
weight: 70
description: "Every write-serialised site, the invariant it protects, and the lock Postgres takes for it."
---

SQLite has no row locks. The Manager serialises every write that spans more
than one statement with `BEGIN IMMEDIATE`, which takes the database write lock
at transaction start. Postgres has no equivalent, so each site that protects a
**named invariant** takes the row or advisory lock that protects it and holds
it until commit. This page is the inventory; the helpers live in
`crates/manager/src/store/locks.rs`.

Rules for adding one:

- A transaction that protects an invariant gets a name — either a dedicated
  function in `crates/manager/src/store` (`cancel_job_in_tx`, `reserve_job`,
  `claim_cron_fire`) or a clearly-scoped block in the one handler that owns
  the operation. New named operations belong in `store`; `api/` may not
  contain SQL (enforced by `scripts/check_store_boundary.sh`).
- Re-check inside the transaction anything that was read before it. A row lock
  orders the writers, but the re-check is what makes the earlier read current.
- Post-commit work (notify, WebSocket fan-out, `wake_all`) never runs inside
  the transaction.
- Every `BEGIN IMMEDIATE` site that reads before it writes takes its Postgres
  lock through `store::locks` — `lock_pipeline_tx`, `lock_job_tx`,
  `lock_runner_tx`, or `lock_first_admin_setup_tx`. On SQLite these helpers are
  ordinary reads; the writer lock already covers the transaction.
- **Lock order is pipeline → job.** `disable`, the trigger path, and
  `approve`/`reject` all take the pipeline row first. `cancel_job_in_tx` runs
  after a pipeline lock and never the reverse, so no two transactions take the
  same pair in opposite order.
- Every lock statement is tagged `Sql::Dual` with a SQLite body, so it is
  compiled by the SQLite validator and `PREPARE`d against the Postgres schema
  by the PG compile gate.

## Inventory

| Site | Invariant | Serialisation on Postgres |
|------|-----------|---------------------------|
| `api/auth.rs::setup` | Exactly one first platform admin, even under concurrent `/setup` | transaction-scoped advisory lock (`lock_first_admin_setup_tx`); no user row exists to lock |
| `store/trigger_reserve.rs::reserve_job` | Gapless per-pipeline `run_number`; a concurrent disable cannot slip a job in; the concurrency decision is atomic with the insert | pipeline row lock, in-transaction concurrency re-resolve, then the atomic `UPDATE pipelines … RETURNING run_counter` |
| `store/pipelines.rs::resolve_concurrency_status(_in_tx)` | Pipeline concurrency decision (`pending` / `queued` / discard) is serialised with job creation and approval promotion | pipeline row lock (re-entrant when the caller already holds it) |
| `services/pipeline_scanner.rs::upsert_from_yaml` | Definition update + task/resolution projection + ETag compare are one write; team move only with no active jobs | pipeline row lock + the guarded `WHERE yaml_source = ?` compare-and-set |
| `store/jobs.rs::cancel_job_in_tx` | Cancel flips status, marks the running step, records the outcome atomically | guarded `UPDATE … WHERE status <> terminal` (no extra lock) |
| `store/jobs.rs::approve` / `reject` | Approval count + threshold promotion is one decision; two approvers cannot both promote or leave a satisfied job waiting | pipeline row lock, then job row lock; `UNIQUE(job_id, user_id, action)`; guarded `UPDATE … WHERE status = 'waiting_approval'` |
| `store/jobs.rs::insert_rerun` | New attempt row + `run_attempt` increment + old job untouched; concurrency decision atomic with the insert | pipeline row lock, in-transaction concurrency re-resolve, `UNIQUE(pipeline_id, run_number, run_attempt)` |
| `store/jobs.rs::apply_runner_status` | Runner-only writes; terminal states win; step-status updates cannot resurrect a cancelled job | job row lock, then guarded `UPDATE … WHERE id = ? AND status NOT IN (terminal)` |
| `api/runners.rs::poll_once` (claim) | Candidate re-read, team check, slot gate, environment gate and the claim `UPDATE` are one decision; serialised with team-binding moves and freeze toggles | runner row lock, `FOR UPDATE` on the `sandbox_hosts` row for slot occupancy, atomic `UPDATE jobs … WHERE status = 'pending' RETURNING` for the claim |
| `store/runners.rs::change_execution_mode` | Transition validation and the mode write share one scope | runner row lock |
| `store/runners.rs::toggle_freeze` | Read-modify-write of the freeze flag | runner row lock (claims take the same lock) |
| `store/runners.rs::move_team` | Team binding moves only while no job is running | runner row lock (claims take the same lock before starting a job) |
| `store/pipelines.rs::disable` | Flip `is_active` before cancelling in-flight jobs so a trigger cannot slip in | pipeline row lock before the read |
| `store/pipelines.rs::enable` | Flip + audit are one write | single-row `UPDATE` |
| `store/pipelines.rs::update_guarded` | Structured pipeline update + team-move re-check | guarded `UPDATE` (compare-and-set) |
| `store/plugin_sources.rs::toggle_pool_entry` | Pool entry enable/disable + index consistency | single-row `UPDATE` |
| `services/cron_scheduler.rs::claim_cron_fire` | One firing per (pipeline, window) | pipeline row lock around the last-fired read and stamp |
| `services/job_log_retention.rs::purge_expired_job_logs` | Delete chunks and stamp the job together so a purge is not half-applied | one transaction (`DELETE` + `UPDATE`) |
| `jobs.seq` / `audit_logs.seq` triggers | Concurrent inserts receive distinct, dense sequence numbers | transaction-scoped advisory lock inside the `AFTER INSERT` trigger (Postgres migration 2) |

`services/scheduler.rs` additionally owns queue promotion and queue/approval
expiry; those are single-statement `UPDATE`s with a `WHERE status = …` guard
and need no explicit lock.

## Scope

This build serves **one Manager per storage** (`[cluster] mode = "single"`;
`multi` is refused at startup), so these locks order concurrent requests and
background loops inside one process. They are row-scoped rather than global, so
they remain correct if active-active serving is added later — the missing piece
for `multi` is cross-instance coordination of the scheduler and leases, not the
per-row invariants on this page.
