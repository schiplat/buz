---
title: "Storage & Cluster Mode"
description: "SQLite and Postgres backends, [cluster] mode, and the single-instance lock"
weight: 42
---

## Storage backends

Buz's Manager keeps all state in one SQL database. Two backends are supported:

| Backend | Status | `[cluster] mode` | Build |
|---|---|---|---|
| SQLite (bundled) | **served** | `single` | default |
| Postgres (14+) | **served** | `single` | `--features postgres` |

`multi` is reserved for a future active-active mode: this build refuses it at
startup for both backends. SQLite has no cross-instance coordination, and the
Postgres backend holds a session advisory lock on its storage, so a second
Manager against the same database is refused before it serves a request.

## Configuration

```toml
[database]
# Legacy alias — a plain file path. Used when [database] url is absent.
path = "data/manager.db"

# Explicit connection target. When set, it wins over `path`.
# sqlite:<path> works in the default build; postgres://… requires the
# `postgres` cargo feature.
# url = "sqlite:data/manager.db"
# url = "postgres://buz:secret@db.internal:5432/buz"

max_connections = 8

[cluster]
# single (default, and the only mode this build serves)
mode = "single"
```

Commands that open the SQLite file directly resolve the same target as the
server: `[database] url` when present, otherwise the legacy `path`. This
matters after an in-place switch, when the legacy file is stale —
`migrate-sqlite-to-pg` refuses to guess and requires `--sqlite-path` if the
config already points at Postgres.

## What the startup gate enforces

The Manager validates `[cluster] mode` against the configured backend before
anything touches storage, and refuses to start otherwise:

- **`mode = "multi"` → refused on every backend.** This build serves one
  Manager per storage and holds an exclusive lock on it. SQLite additionally
  has no cross-instance coordination (a single writer, no row locks).
- **SQLite + `single` → the single-instance lock.** The Manager takes an
  advisory flock on a `<db file>.lock` sidecar and holds it for the process
  lifetime. A systemd restart stacked on the old process, or a hand-started
  second Manager, is refused before it can touch storage. The lock file is
  intentionally never deleted — deleting it would race with the next
  acquirer.
- **Postgres + `single` + `postgres` feature → served.** The schema, statement
  provenance gates, dual-backend dispatcher, PG migration runner, pool
  selection (`db::init` returns a `DbPool`), and the Postgres session advisory
  lock are wired. Every store statement prepares on a real Postgres 14
  server. A binary compiled without the feature refuses a Postgres url (fail
  closed, never silently falls back to SQLite).

## How the two backends stay isolated

Every statement in `store::` carries its dialect provenance (`store::sql::Sql`):

- **Shared** — dialect-neutral text used on both backends. The only per-backend
  translation is `?` → `$n` placeholder renumbering; the syntax itself must be
  portable, which a lint gate enforces (json walks, `INSERT OR …`,
  `strftime` epochs and friends fail the build if they appear in a shared
  statement).
- **Dual** — the handful of statements with no portable spelling (the jsonb
  walks and epoch math in the runner claim path, plus the explicit row locks
  in `store::locks`). Each carries a SQLite body and a Postgres body side by
  side; both must accept the same binds in the same order, and both are
  compile-checked against their schema.
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

## Migrating an existing SQLite deployment to Postgres

1. **Upgrade the binary and let SQLite finish its chain.** Start this build
   once in `single` mode against the existing SQLite file. It applies schema
   versions through v3 (including `pipelines.yaml_source`) and runs the
   one-time definition backfill. Wait for `Database migrations applied
   successfully` and `definition backfill finished`, then stop the Manager.
2. **Keep the same `auth.jwt_secret`.** Stored secrets are encrypted with a key
   derived from it; a different secret makes every copied ciphertext
   undecryptable.
3. **Provision an empty Postgres database** (14+). The tool applies the
   schema baseline; the target must carry no application rows.
4. **Plan the copy:**
   `buz-manager migrate-sqlite-to-pg --config manager.toml --pg-url postgres://…`
   (the url may come from `BUZ_PG_URL`). For a config whose `[database] url`
   already points at Postgres, pass `--sqlite-path` explicitly.
5. **Apply:** add `--apply`. The command copies the 38 application tables in
   foreign-key order inside one transaction, verifies per-table counts before
   committing, never overwrites a populated target, and refuses a source whose
   `schema_migrations` is not at this build's version.
6. **Switch the config:** point `[database] url` at `postgres://…`, keep
   `[cluster] mode = "single"`, and start a binary built with the `postgres`
   feature.
7. **Keep `yaml_scan_dir` available for the first start.** The startup backfill
   prefers the exact legacy YAML file when it exists; the directory is never
   watched or written by the server.

## Operational notes

- **Official release binaries are SQLite-only.** They are built without the
  `postgres` feature, so `postgres:` urls and `migrate-sqlite-to-pg` fail
  closed. Build from source with `--features postgres` to serve Postgres.
- **`rotate-jwt-key` is SQLite-only.** It refuses a Postgres config instead of
  rotating a stale SQLite file. Postgres key rotation is not implemented.
- **CI coverage:** the `postgres` job runs the journey, parity, and statement
  compile gates, the 47-suite dual-backend fixture matrix, the SQLite→Postgres
  migration test, and the concurrency gates (`pg_concurrency_test`) against a
  real Postgres 14 server.

## Implementation status

1. **Runtime wiring — done.** `db::init` produces a `DbPool`; `AppState.pool`
   is backend-neutral; `single` + Postgres takes a session advisory lock.
2. **Services SQL convergence — done.** Every service statement lives in
   `store::`; only `rotate_key` (a deliberately SQLite-only CLI using
   `VACUUM INTO` + table rotation) stays outside.
3. **Postgres enablement — done.** Shared statements are portable (the
   rewriter only renumbers placeholders), the dual statements carry explicit
   PG bodies, the whole corpus prepares on a real server, and the CI
   Postgres job is green. The `BUZ_TEST_DB` fixture matrix runs 47 suites on
   Postgres.
4. **Read-then-write serialisation — done.** Every `BEGIN IMMEDIATE` invariant
   has an explicit Postgres row or advisory lock; see
   [Transaction Boundaries](/docs/01-architecture/execution/transaction-boundaries/).
   The slot-budget write skew was the last one found and is covered by the
   guarded host-row reconcile.
5. **In-place backend switch — done.** `buz-manager migrate-sqlite-to-pg` copies
   a fully migrated SQLite database into an empty Postgres target: one
   transaction, foreign-key order, per-table count verification before the
   commit, occupancy guard, and a source-schema version gate. Plan only
   without `--apply`.
