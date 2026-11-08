---
title: Job Concurrency & Queue
weight: 90
description: "Design for pipeline-level concurrency control with queue, discard-old, and discard-new modes — ready for multi-runner and multi-manager clusters."
aliases:
  - /docs/01-architecture/job-concurrency-queue/
---

{{< callout type="note" >}}**Design record — implemented.** Schema and scheduler sections reflect shipped behavior; API-change lists are historical notes.{{< /callout >}}

## Status

**Implemented** in Manager trigger (`crates/manager/src/api/pipelines.rs`) and
surfaced on **Job Detail**.

Related: YAML schema in [Pipeline Format](/docs/01-architecture/authoring/pipeline-format/).

### Job Detail UX

When a job is created, the snapshot freezes:

- `concurrency_group`
- `concurrency_mode` (effective mode used at trigger)
- `concurrency_max_queue` / `concurrency_queue_timeout_seconds` (when a group is set)

Job Detail shows:

1. **Queued banner** (`status === queued`) — this run is waiting for an earlier
   job in the group; only one runs at a time (not “you cannot trigger again”).
2. **Concurrency panel** — **always visible**:
   - **Configured:** frozen group + mode (+ max-queue / timeout for `queue`),
     mode hint, help disclosure linking to
     `/pipelines/{id}/definition` (**Definition → Edit**). Snapshot is immutable
     for this job; edits apply to **future** triggers only.
   - **Not configured** (no `concurrency` in YAML / empty group): status
     `not configured`, shows platform defaults if enabled later
     (`discard-old`, max-queue `3`, queue-timeout `30m`), explains parallel
     triggers are allowed, and lists the same adjustable fields + definition link.

Older jobs may lack `concurrency_mode` in the snapshot; the UI still shows the
group and notes that the mode was not frozen.

### Pipeline Detail (Definition)

The Definition tab always shows a **Concurrency** strip (below Trigger / Runner /
Bindings / Config). For `source=yaml` pipelines the whole Definition (including
concurrency) is parsed from the on-disk file; invalid/missing files show an alert
instead of the stale DB projection:

- **Configured:** live `group` / `mode` / (for `queue`) max-queue + timeout, hint, help.
- **Not configured:** same defaults preview as Job Detail + how to enable.
- **Edit YAML** on Definition (or Pipelines list) opens the online editor; Save writes `yaml_scan_dir` and upserts immediately. See [YAML-First Authoring](/docs/01-architecture/authoring/yaml-first-authoring/).

---


## Problem

When a pipeline is triggered multiple times in rapid succession (e.g. GitHub push webhook
fires 3 times in 2 seconds), the system must decide:

1. Run all jobs in **parallel**? (wasteful, risk of race conditions)
2. **Reject** duplicates? (current behavior — 409 Conflict)
3. **Queue** them FIFO?
4. **Discard old** queued jobs, keep only the latest?

The answer depends on the pipeline's purpose: a deployment should queue,
a CI build should discard-old, and a cron cleanup can safely discard-new.

Current behavior (`concurrency_group` without mode) is **discard-new**:
if any pending/running job exists, new triggers get HTTP 409.

---

## Modes

| Mode | Behavior | Use Case |
|---|---|---|
| **`discard-old`** (default, can be omitted) | Cancel all queued jobs for this group, keep only the newest waiting. A running job is never cancelled. | CI builds — only the latest commit matters |
| **`queue`** | FIFO queue up to `max-queue` depth. Jobs wait their turn. | Deploy pipelines — every release must run |
| **`discard-new`** | Reject new triggers if any job is pending/running. Equivalent to current 409 behavior. | Cleanup / cron tasks |

When `mode` is omitted from YAML, the default is `discard-old`. For example:

```yaml
# These two are equivalent:
concurrency:
  group: dind-build

concurrency:
  group: dind-build
  mode: discard-old
```

### Decision tree (trigger time)

```
trigger arrives
  │
  ├─ any running job in concurrency group?
  │   └─ yes → mode?
  │       ├─ discard-old → CANCEL all queued, ENQUEUE this one, return 202
  │       ├─ queue        → ENQUEUE if below max-queue, return 202
  │       └─ discard-new  → RETURN 409 Conflict
  │
  └─ no running job → dispatch immediately (pending), return 201
```

### Visual: discard-old

```
t=0   trigger A → running
t=1   trigger B → queued   (A still running)
t=2   trigger C → B cancelled, C queued  (only C remains)
t=3   A finishes → C dispatched
```

### Visual: queue

```
t=0   trigger A → running
t=1   trigger B → queued #1
t=2   trigger C → queued #2  (if max-queue ≥ 2)
t=3   A finishes → B dispatched
t=4   B finishes → C dispatched
```

---

## YAML Syntax

```yaml
name: CI Build and Push
description: DinD build pipeline with queue control.

# ── Concurrency control ──
concurrency:
  group: dind-build           # required — shared across pipelines with same group name
  # mode: discard-old         # default (can be omitted) — queue | discard-new
  # max_queue: 3              # default: 3, only for mode=queue (alias: max-queue)
  # queue_timeout: 10m        # default: 30m (alias: queue-timeout)

on:
  manual:
    inputs:
      version:
        description: Version tag
        required: true

runner:
  labels: [linux-x64, docker, dind]
  timeout: 30m

steps:
  - name: Build
    engine: docker
    image: docker:27-dind
    privileged: true
    run: |
      dockerd &
      sleep 3
      docker build -t myapp:$version .
    timeout: 15m
```

**Backward compatibility:** 

1. Existing `concurrency_group: "some-group"` **string** format is preserved as-is. Pipelines with the old format continue to work with `discard-new` behavior (current 409).
2. When a pipeline adopts the new `concurrency:` block, existing `concurrency_group` is ignored in favor of `concurrency.group`.
3. When pipeline has `concurrency:` block but omits `mode`, the default is `discard-old`. 

This means existing pipelines are NOT automatically migrated to the new default — they keep their old behavior. Only newly created or edited pipelines using the `concurrency:` block get `discard-old` as default.

---

## DB Schema

### `pipelines` — new columns

```sql
ALTER TABLE pipelines ADD COLUMN concurrency_mode TEXT NOT NULL DEFAULT 'discard-old';
ALTER TABLE pipelines ADD COLUMN concurrency_max_queue INTEGER NOT NULL DEFAULT 3;
ALTER TABLE pipelines ADD COLUMN concurrency_queue_timeout_seconds INTEGER NOT NULL DEFAULT 1800;
```

Existing `concurrency_group TEXT` column is reused. When `concurrency_group` is NULL, no concurrency control applies (unlimited parallel).

### `jobs` — new column + status value

```sql
ALTER TABLE jobs ADD COLUMN queue_position INTEGER;
-- status: add 'queued' as a valid value
-- queued jobs have queue_position > 0; dispatched jobs set it to NULL
```

### `scheduler_lease` — new table (for cluster support)

```sql
CREATE TABLE IF NOT EXISTS scheduler_lease (
    id INTEGER PRIMARY KEY CHECK (id = 1),   -- singleton row
    instance_id TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

A single-row table used for leader election across multiple Manager instances.
Only the instance holding a valid lease runs the scheduler loop.

---

## Job Lifecycle (extended)

```
                          ┌─────────┐
                          │ trigger │
                          └────┬────┘
                               │
                    ┌──────────┴──────────┐
                    │  active job exists? │
                    └──────────┬──────────┘
                          yes  │         no
               ┌───────────────┤         └──→ pending → running → success/failed
               │               │
    ┌──────────┴──────────┐    │
    │  mode?              │    │
    ├── discard-old       │    │
    │   cancel queued     │    │
    │   enqueue new       │    │
    ├── queue             │    │
    │   enqueue if < max  │    │
    │   or 429 Too Many   │    │
    └── discard-new       │    │
        409 Conflict      │    │
                          │    │
                          ▼    │
                      ┌────────┴──┐
                      │  queued   │
                      └────┬──────┘
                           │
               ┌───────────┼───────────┐
               │           │           │
        queue_timeout   cancelled   scheduler
               │       (manually)   picks next
               ▼                     │
          cancelled              ┌───┴───┐
                                 │pending│
                                 └───┬───┘
                                     │
                              ┌──────┴──────┐
                              │   runner    │
                              │  claims job │
                              └──────┬──────┘
                                     │
                                ┌────┴────┐
                                │ running │
                                └────┬────┘
                                     │
                              ┌──────┴──────┐
                              │ success /   │
                              │ failed      │
                              └─────────────┘
```

---

## Scheduler Design

### Problem

A `queued` job must eventually become `pending` when:
1. No active job exists in the same concurrency group
2. A matching runner is available (future: multi-runner)

The transition cannot happen inside the `POST /trigger` handler because
the active job may finish seconds later — the trigger handler has no
visibility into the future.

### Architecture

```
┌─────────────────────────────────────────────────────┐
│  Manager Cluster                                    │
│                                                     │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐ │
│  │ Instance 1  │  │ Instance 2  │  │ Instance 3  │ │
│  │ (lease      │  │             │  │             │ │
│  │  holder)    │  │             │  │             │ │
│  │   ┌───────┐ │  │             │  │             │ │
│  │   │Sched  │ │  │             │  │             │ │
│  │   └───┬───┘ │  │             │  │             │ │
│  └───────┼─────┘  └─────────────┘  └─────────────┘ │
│          │                                          │
│          │  SELECT queued jobs GROUP BY concurrency │
│          │  UPDATE status = 'pending' (atomic)      │
│          ▼                                          │
│  ┌───────────────────┐                              │
│  │  SQLite (WAL)     │                              │
│  │  jobs + lease     │                              │
│  └───────────────────┘                              │
└─────────────────────────────────────────────────────┘
```

### Leader Election

Every Manager instance starts a background Tokio task. Only the lease holder runs the dispatch loop:

```rust
// Pseudocode
loop {
    sleep(Duration::from_secs(2));

    // Try to acquire lease
    let acquired = sqlx::query(
        "UPDATE scheduler_lease
         SET instance_id = ?, expires_at = datetime('now', '+5 seconds'), updated_at = datetime('now')
         WHERE expires_at < datetime('now')"
    ).bind(my_instance_id).execute(&pool).await?;

    if acquired.rows_affected() == 0 {
        continue; // Another instance holds the lease
    }

    // Lease acquired — run dispatch logic
    dispatch_queued_jobs(&pool).await;
}
```

If the lease-holding instance crashes, the lease expires after 5 seconds and another instance takes over. No manual failover needed.

### Dispatch Logic

One DB transaction per concurrency group to prevent race conditions:

```sql
BEGIN TRANSACTION;

-- Count active jobs for this concurrency group
SELECT COUNT(*) FROM jobs
WHERE pipeline_id = ? AND status IN ('pending', 'running');

-- If active == 0, promote one queued job
UPDATE jobs SET status = 'pending', queue_position = NULL
WHERE id = (
    SELECT id FROM jobs
    WHERE pipeline_id = ? AND status = 'queued'
    AND concurrency_group = ?
    ORDER BY created_at ASC
    LIMIT 1
);

COMMIT;
```

### Queue Timeout Cleanup

Same scheduler loop drains expired queued jobs:

```sql
UPDATE jobs SET status = 'cancelled', queue_position = NULL
WHERE status = 'queued'
AND created_at < datetime('now', '-' || queue_timeout_seconds || ' seconds');
```

### Multi-Runner Awareness (future)

When dispatching from queued → pending, check runner availability:

```sql
-- Only dispatch if a matching runner is online
SELECT COUNT(*) FROM runners
WHERE status = 'online'
AND labels_json MATCH pipeline_runner_labels
```

If no matching runner, keep the job queued. Don't transition to pending
(prevents a job sitting in "pending" that no runner can claim).

---

## API Changes

### Trigger response

| Trigger outcome | HTTP | Body |
|---|---|---|
| Dispatched immediately | 201 | `{ job_id, status: "pending" }` |
| Queued (discard-old / queue) | 202 | `{ job_id, status: "queued", position: 2 }` |
| Rejected: max-queue full | 429 | `{ error: "queue full (max 3)" }` |
| Rejected: discard-new mode | 409 | `{ error: "1 active job(s)" }` (unchanged) |

### Cancel queued job

```
POST /api/v1/jobs/{job_id}/cancel
```

Allowed for `queued` jobs. Sets status to `cancelled`, frees the queue slot.

---

## Frontend

### Pipeline Detail → Latest Job Status

| State | Badge |
|---|---|
| `queued` | 🔵 "Queued (#2)" |
| `pending` | 🟡 "Pending" |
| `running` | 🟢 "Running" |

### Job Detail — Queued View

When `status === 'queued'`:
- Show queue position and estimated wait
- Provide "Cancel" button
- Disable the log viewer (no runner yet)
- Auto-poll for status change every 2 seconds

### Trigger Confirmation

After clicking "Trigger" in the UI, show a toast:

```
✅ Job queued — you are #2 in "dind-build"
   Previous job is still running. Your job will start automatically.
```

---

## Concurrency Group Sharing

Multiple pipelines can share a `concurrency_group`, e.g. all `staging-*` pipelines
use `group: staging` to prevent parallel deploys to the same environment:

```yaml
# pipeline staging-api.yml
concurrency:
  group: staging
  mode: queue

# pipeline staging-worker.yml
concurrency:
  group: staging
  mode: queue
```

Both pipelines check the same concurrency_group in the DB query. The `pipeline_id`
filter in the dispatch SQL is replaced with `concurrency_group = ?`.

---

## Comparison with Industry

| Tool | Modes | Default |
|---|---|---|
| **Jenkins** | Do-not-allow-concurrent-queue / Discard-old / Discard-new / Throttle | Queue |
| **GitHub Actions** | `concurrency.group` + `cancel-in-progress` (bool) | Cancel-in-progress = false (queue) |
| **GitLab CI** | `resource_group` + `process_mode` (unordered / oldest_first / newest_first) | Unordered |
| **Buz (design)** | queue / discard-old / discard-new | discard-old |

**Why discard-old as default:** Buz's primary use case is CI/CD triggered by git
pushes. In that scenario, only the latest commit matters — older queued builds are
wasted resources. This matches GitHub Actions' `cancel-in-progress: true` behavior
and avoids Jenkins' notorious queue-bloat problem.

---

## Implementation Plan

| Phase | What | Complexity |
|:-----:|------|:----------:|
| 1 | DB migration: `concurrency_mode`, `max_queue`, `queue_timeout_seconds`, `queue_position`, `scheduler_lease` | Low |
| 2 | Trigger handler: mode dispatch logic (discard-old, queue, discard-new) | Medium |
| 3 | Scheduler: leader lease + dispatch loop + timeout cleanup | Medium |
| 4 | API: queued-status responses, cancel queued jobs | Low |
| 5 | Frontend: queued status in job list/detail, trigger confirmation toast | Medium |
| 6 | Multi-runner awareness: check runner availability before dispatch | Low |
| 7 | Multi-pipeline concurrency group sharing | Low |

---

Boundary: this page is the **pipeline-level** declaration (what a pipeline allows). The **Runner-level** policy (what a runner machine permits — max jobs, engines, cleanup) is UI-managed: see [Runner Policy](/docs/01-architecture/execution/runner-policy/).
