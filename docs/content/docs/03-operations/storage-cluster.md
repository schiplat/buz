---
title: "Storage & Cluster Mode"
description: "Storage backends, [cluster] mode, and the single-instance lock"
weight: 42
---

## Storage backends

Buz's Manager keeps all state in one SQL database. Two backends are planned:

| Backend | Status | `[cluster] mode` |
|---|---|---|
| SQLite (bundled) | **served today** | `single` only |
| Postgres (14+) | backend wired, enablement pending | `multi` target |

## Configuration

```toml
[database]
# Legacy alias — a plain file path. Used when [database] url is absent.
path = "data/manager.db"

# Explicit connection target. When set, it wins over `path`.
# sqlite:<path> is served today; postgres:<…> is refused at startup until the
# Postgres backend ships.
# url = "sqlite:data/manager.db"

max_connections = 8

[cluster]
# single (default): this process is the only Manager serving this storage.
# multi: several Managers serve active-active against one Postgres backend.
mode = "single"
```

## What the startup gate enforces

The Manager validates `[cluster] mode` against the configured backend before
anything touches storage, and refuses to start otherwise:

- **`multi` + SQLite → refused.** SQLite has no cross-instance coordination
  (a single writer, no row locks), so two Managers on one file would corrupt
  scheduling state — gapless build numbers, slot budgets, and claim
  serialisation all rely on the write lock `BEGIN IMMEDIATE` takes. `multi`
  requires the Postgres backend.
- **Postgres → refused (today).** The schema, statement provenance gates,
  dual-backend dispatcher, PG migration runner, pool selection (`db::init`
  returns a `DbPool`), `AppState.pool`, and the Postgres single-instance
  advisory lock have landed, and every store statement is proven to prepare
  on a real Postgres server (the CI compile gate `PREPARE`s the whole corpus).
  Enablement still waits on the end-to-end journey set being exercised on a
  real Postgres server and on the CI Postgres job's first green run. The
  refusal stays fail closed: a config pointing at Postgres never silently
  runs on SQLite.
- **`single` + SQLite → the single-instance lock.** The Manager takes an
  advisory flock on a `<db file>.lock` sidecar and holds it for the process
  lifetime. A systemd restart stacked on the old process, or a hand-started
  second Manager, is refused before it can touch storage. The lock file is
  intentionally never deleted — deleting it would race with the next
  acquirer.

## How the two backends stay isolated

Every statement in `store::` carries its dialect provenance (`store::sql::Sql`):

- **Shared** — dialect-neutral text used on both backends. The only per-backend
  translation is `?` → `$n` placeholder renumbering; the syntax itself must be
  portable, which a lint gate enforces (json walks, `INSERT OR …`,
  `strftime` epochs and friends fail the build if they appear in a shared
  statement).
- **Dual** — the handful of statements with no portable spelling (the jsonb
  walks and epoch math in the runner claim path) carry an explicit SQLite body
  and an explicit Postgres body side by side; both must accept the same binds
  in the same order, and both are compile-checked against their schema.
- **Sqlite-only / Postgres-only** — statements that exist on one backend fail
  closed on the other instead of mis-executing.

Every shared and Postgres statement is additionally `PREPARE`d against the
migrated Postgres schema in CI (`pg_sql_compile_test`), so a statement that
only compiles on SQLite can no longer reach a runtime endpoint.

## Why the timestamps stay text

Both dialects store timestamps as TEXT in `YYYY-MM-DD HH24:MI:SS` format, and
every timestamp comparison in the shared SQL strings is a text comparison
against that exact format. On Postgres, the schema installs `datetime()` shim
functions that produce the identical text (via `to_char`, `'now'` accepted as
an input), so the `datetime(...)` calls in the shared statements are valid on
both backends with identical semantics. This is a deliberate portability
trade: the alternative (`timestamptz` columns) would require rewriting every
statement that touches a timestamp.

## Roadmap

1. **Runtime wiring — done.** `db::init` produces a `DbPool` (Postgres: pool
   plus `run_pg_migrations`; SQLite: the existing PRAGMA setup), `AppState.pool`
   is backend-neutral, and `single` + Postgres takes a session advisory lock.
2. **Services SQL convergence — done.** Every service statement now lives in
   `store::`; only `rotate_key` (a deliberately SQLite-only CLI using
   `VACUUM INTO` + table rotation) stays outside.
3. **Postgres enablement** — statement-level work is done: every shared
   statement is portable (the rewriter only renumbers placeholders), the six
   dual statements carry explicit PG bodies, and the whole corpus prepares on
   a real server. Remaining: the end-to-end journey set on Postgres, the
   `BUZ_TEST_DB` matrix, and the CI Postgres job's first green run; then
   `validate_cluster_mode` lifts the Postgres refusal.
4. **`multi` enablement** — per-site serialisation review
   (`transaction-boundaries.md`), scheduler coordination, and the removal of
   the single-instance lock on Postgres.
