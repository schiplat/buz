---
title: Transaction Boundaries
weight: 70
description: "Every BEGIN IMMEDIATE site, the invariant it protects, and the Postgres translation it will need."
---

SQLite has no row locks. The Manager serialises every write that spans more
than one statement with `BEGIN IMMEDIATE`, which takes the database write lock
at transaction start. Each of those sites protects a **named invariant**; this
page is the inventory to review before a storage change, and the checklist for
moving to Postgres.

Rules for adding one:

- A transaction that protects an invariant gets a name — either a dedicated
  function in `crates/manager/src/store` (`cancel_job_in_tx`, `reserve_job`,
  `claim_cron_fire`) or a clearly-scoped block in the one handler that owns
  the operation. New named operations belong in `store`; `api/` may not
  contain SQL (enforced by `scripts/check_store_boundary.sh`, allowlist at
  `scripts/store_boundary_allowlist.txt`).
- Re-check inside the transaction anything that was read before it. The write
  lock only orders writers; it does not make an earlier read current.
- Post-commit work (notify, WebSocket fan-out, `wake_all`) never runs inside
  the transaction.
- The dialect token itself — SQLite's `BEGIN IMMEDIATE` — has exactly one
  home: `store::tx::begin_immediate`. A grep for `begin_immediate` enumerates
  every write-serialised site; the Postgres port replaces that one function's
  body with the per-site serialisation strategy (row locks, `SKIP LOCKED`,
  advisory locks) instead of hunting for the string.

## Inventory

| Site | Invariant | Postgres translation |
|------|-----------|----------------------|
| `services/trigger_reserve.rs::reserve_job` | Gapless per-pipeline `run_number`; `is_active` re-checked so a concurrent disable cannot interleave | `INSERT ... ON CONFLICT` on a `(pipeline_id, run_number)` unique key, or `SELECT ... FOR UPDATE` on the pipeline row |
| `api/pipelines.rs::resolve_concurrency_status_in_tx` | Pipeline concurrency decision (`pending` / `queued` / discard) is serialised with job creation | advisory lock keyed by `pipeline_id`, or `FOR UPDATE` on the pipeline row |
| `services/pipeline_scanner.rs::upsert_from_yaml` | Definition update + task/resolution/binding projection + ETag compare are one write | `FOR UPDATE` on the pipeline row; ETag compare-and-set stays in the same transaction |
| `store/jobs.rs::cancel_job_in_tx` | Cancel flips status, marks the running step, records approval outcome atomically | `UPDATE ... WHERE status <> terminal` (no lock needed) |
| `api/jobs.rs::approve` / `reject` | Approval row + status transition + snapshot update | unique `(job_id, user_id)` + `UPDATE ... WHERE status = 'waiting_approval'` |
| `api/jobs.rs::rerun` | New attempt row + `run_attempt` increment + old job untouched | unique `(pipeline_id, run_number, run_attempt)` |
| `api/jobs.rs::update_status` | Runner-only writes; terminal states win; truncation metadata is a union | `UPDATE ... WHERE id = ? AND runner_id = ? AND status NOT IN (terminal)` |
| `api/runners.rs::poll_once` (claim) | Candidate re-read, team check, slot gate, environment gate and the claim `UPDATE` are one decision | `SELECT ... FOR UPDATE SKIP LOCKED` on pending candidates; slot accounting as a row lock |
| `api/runners.rs::update_runner` | System-info and policy updates replace one runner row without clobbering unrelated columns | single-row `UPDATE` |
| `api/pipelines.rs::disable` | Flip `is_active` before cancelling in-flight jobs so a trigger cannot slip in | `UPDATE ... WHERE id = ?` + `FOR UPDATE` on the pipeline row |
| `api/pipelines.rs::enable` | Flip + audit are one write | single-row `UPDATE` |
| `api/pipelines.rs::update` | Structured pipeline update + audit | single-row `UPDATE` |
| `api/plugin_sources.rs::toggle_pool_entry` | Pool entry enable/disable + index consistency | single-row `UPDATE` |
| `services/cron_scheduler.rs::claim_cron_fire` | One firing per (pipeline, minute) across schedulers | unique `(pipeline_id, fired_at)` insert |
| `services/job_log_retention.rs::purge_expired_job_logs` | Delete chunks and stamp the job together so a purge is not half-applied | `DELETE ... RETURNING` in one transaction |

`services/scheduler.rs` additionally owns queue promotion and queue/approval
expiry; those are single-statement `UPDATE`s with a `WHERE status = ...` guard
and need no explicit lock.

## Why this matters for the storage switch

SQLite's `BEGIN IMMEDIATE` is a **global** writer lock: every site above is
already fully serialised against every other one. That is correct but
coarse — and it is also why "does this transaction really need the lock?" has
never had to be answered.

Postgres replaces the global lock with row/advisory locks. Porting is therefore
not a syntax change but a per-site decision: which row is the serialisation
point? Sites whose only guard is `WHERE status = ...` need nothing; sites that
read-then-write need an explicit lock or a unique constraint. The table above
is that decision, one row per site.
