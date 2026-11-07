---
title: GitHub Actions Secrets/Variables Sync
weight: 50
description: Bind Buz secrets/vars to GitHub repos, push values to Actions
---

# GitHub Actions Secrets/Variables Sync

## Background

Free GitHub Actions minutes are org-scoped. When an org runs out of build time,
a common workaround is to create a **new org** (fresh quota) and migrate each
repo. Every repo's **Actions secrets & variables** then need to be re-created —
a manual, error-prone chore.

Buz already stores the secrets and variables that back these repos. This feature
extends the Buz **Secrets** and **Variables** management areas: a secret /
variable can be **bound to one or more GitHub repos**, and its value pushed to
GitHub Actions with one click. Unbinding and re-pushing are first-class
operations.

## Core idea — Buz secret/var is the source of truth

Unlike a generic "copy repo A → repo B" migration tool, this feature is centered
on the Buz resource itself:

```
┌─────────────────────────────┐          PUT /repos/{owner}/{repo}/actions/secrets/{name}
│  Buz Secret POSTGRES_PASSWORD │  ──────────────────────────────────────────────►  github.com/new-org/app-1
│  ──────────────────────────  │          PUT /repos/{owner}/{repo}/actions/secrets/{name}
│  binds:                      │  ──────────────────────────────────────────────►  github.com/new-org/app-2
│   • conn-a / new-org/app-1   │
│   • conn-a / new-org/app-2   │
└─────────────────────────────┘
```

- A **secret / variable** owns a list of **repo bindings**.
- Each binding points at a pre-registered **repo** (which in turn belongs to a
  **connection**) and tracks the last push result (status / timestamp / error).
- **Push** reads the plaintext **server-side** (decrypt the secret / read the
  platform var), seals it with libsodium (`crypto_box_seal`) for secrets, and
  `PUT`/`PATCH`s it to GitHub. The plaintext never crosses the frontend.
- **GitHub secrets are write-only** — values cannot be read back. That is fine
  here: Buz already holds the values, so Buz is the natural source of truth and
  GitHub is a pure sync target.

## Scope

In scope (v1):
- GitHub **connection** management (encrypted PAT, optional GHE base URL).
- **Repo** management — pre-register target repos under a connection.
- **Fetch repos from GitHub** — batch-pull all repos of an owner (org or user)
  into the repo table, **manually** (one-shot button) **and/or on a
  per-connection schedule**. Fetch is **upsert-only** (adds/refreshes entries,
  never deletes existing repos or their binds).
- **Bind / unbind** a Buz secret or variable to N registered repos.
- **Push** (create/overwrite) a secret's or variable's value to bound repos;
  per-repo result + last-pushed state.
- Platform-admin UI inside the Secrets & Variables section.

Out of scope (possible later):
- Organization-level and environment-level secrets/variables.
- Delete-sync (removing entries on GitHub that no longer match).

## Data model

### Connections (platform-level credentials)

```sql
CREATE TABLE IF NOT EXISTS github_connections (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,                              -- e.g. "buz-org-writer"
    api_base_url TEXT NOT NULL DEFAULT 'https://api.github.com',  -- GitHub Enterprise Server support
    token_ciphertext TEXT NOT NULL,                         -- crypto::encrypt(token, jwt_secret)
    fetch_owners TEXT NOT NULL DEFAULT '',                  -- comma-separated owners to auto-fetch
    fetch_interval_minutes INTEGER,                         -- poll interval; NULL disables auto-fetch
    last_fetch_at TEXT,                                     -- last successful fetch (manual or auto)
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

The PAT is encrypted at rest with the existing `crypto::encrypt` (AES-256-GCM,
`jwt_secret`). API responses never include the token; audit logs only record
`has_token=true`.

Auto-fetch is per-connection: `fetch_owners` (comma-separated org/user names) +
`fetch_interval_minutes` (poll interval; `NULL`/empty disables). A **manual**
fetch also refreshes `last_fetch_at`, so a manual sync suppresses the next
scheduled run. Pre-existing databases get the three columns via idempotent
`ALTER TABLE` in `db/mod.rs`.

### Repositories (pre-registered, reused across binds)

```sql
CREATE TABLE IF NOT EXISTS github_repos (
    id TEXT PRIMARY KEY,
    connection_id TEXT NOT NULL REFERENCES github_connections(id) ON DELETE CASCADE,
    owner TEXT NOT NULL,
    repo TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(connection_id, owner, repo)
);
```

Repos are pre-registered in the GitHub Sync tab (like connections) so they can
be reused: one repo entry can be bound to many secrets/variables, and bind
dialogs select from this list instead of hand-typing `owner`/`repo`. A repo
belongs to a **connection**, which determines the API base URL and the PAT used
to push to it. Deleting a repo cascades to all its binds.

### Bindings (secret / variable → repo)

```sql
CREATE TABLE IF NOT EXISTS secret_github_binds (
    id TEXT PRIMARY KEY,
    secret_id TEXT NOT NULL REFERENCES secrets(id) ON DELETE CASCADE,
    repo_id TEXT NOT NULL REFERENCES github_repos(id) ON DELETE CASCADE,
    last_pushed_at TEXT,
    last_status TEXT NOT NULL DEFAULT 'pending',            -- pending | ok | failed | stale
    last_error TEXT NOT NULL DEFAULT '',
    override_value TEXT,                                    -- NULL = inherit; secrets: ciphertext
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(secret_id, repo_id)
);

CREATE TABLE IF NOT EXISTS variable_github_binds (
    id TEXT PRIMARY KEY,
    var_id TEXT NOT NULL REFERENCES platform_vars(id) ON DELETE CASCADE,
    repo_id TEXT NOT NULL REFERENCES github_repos(id) ON DELETE CASCADE,
    last_pushed_at TEXT,
    last_status TEXT NOT NULL DEFAULT 'pending',            -- pending | ok | failed | stale
    last_error TEXT NOT NULL DEFAULT '',
    last_op TEXT NOT NULL DEFAULT 'push',                   -- bind | push | unbind
    override_value TEXT,                                    -- NULL = inherit platform_vars.value
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(var_id, repo_id)
);
```

Separate tables for secrets and variables mirror the existing
`pipeline_secrets` / `pipeline_vars` pattern and keep FK cascade simple.
Bindings cascade-delete with their parent secret / variable / connection /
repo. The connection and repo of a bind are resolved via `repo_id` at push
time.

### Dirty marking (`stale`) — value drift without auto-push

Push stays **manual**. When the effective push value changes locally, previously
successful binds are marked `stale` so the UI can show **needs push**; nothing
is written to GitHub until an operator runs Push / Push All.

| Event | Which binds | Result |
|-------|-------------|--------|
| Platform Secret/Variable **default value** changes | Only binds with `override_value IS NULL` (inherit) and `last_status = 'ok'` | → `stale` |
| Platform value change | Binds with a **custom override** | Unchanged (effective value does not depend on the default) |
| Metadata-only secret update (empty `value`) | — | No dirty mark |
| `PATCH` bind override (inherit ↔ custom / custom value) | That bind, if it was `ok` | → `stale` |
| Already `pending` / `failed` / `stale` | — | Left alone (still needs attention) |

`PUT /secrets/{id}` and `PUT /variables/{id}` responses include
`github_binds_stale: N` (count of rows marked). The dashboard prompts
**Push now** / **Later** when `N > 0`. There is **no** automatic async push
after save.

### Variable / secret per-repo override (`override_value`)

Platform Variables and Secrets still have a single shared **default**.
For GitHub Actions sync, each bind row may set `override_value`:

| Resource | Storage | API list / GET bind |
|----------|---------|---------------------|
| Variable | Plaintext in `override_value` | Returns `override_value` + `has_override` |
| Secret | AES ciphertext in `override_value` | Returns `has_override` only (`override_value` always `null`) |

| `override_value` column | Value pushed to that repo |
|-------------------------|---------------------------|
| `NULL` | Inherit platform default (same as v1) |
| non-NULL | Push the override (secrets: decrypt then seal for GitHub) |

GitHub Actions **name** is always the Buz resource name. Push never accepts a
one-shot value in the request body; edit the bind (`PATCH`), then Push.
Resolution: `effective = override ?? default`.

This is **orthogonal** to platform [Environments](./environments/) (Buz job claim overlays).

## Backend design

### New dependency

- `crypto_box` (feature `seal`) — pure-Rust libsodium `crypto_box_seal`-
  compatible sealed box, required to encrypt secret values for GitHub. It uses
  `curve25519-dalek` 4.x, which is compatible with the rest of the workspace
  (the alternative `nacl-sealed-box` pulled in an incompatible
  `curve25519-dalek` 5.0.0-pre and was dropped).
- `reqwest` already exists in the Manager (used for notify webhooks).

### Service — `crates/manager/src/services/github_actions_sync.rs`

- `GitHubClient::new(api_base_url, token)` — thin wrapper over the GitHub REST
  API (JSON).
- `get_repo_public_key(owner, repo) -> (key_id, PublicKey)`.
- `seal_secret_value(public_key, plaintext) -> base64`.
- `put_secret(owner, repo, name, encrypted_value, key_id)`.
- `upsert_variable(owner, repo, name, value)` — `POST` when missing,
  `PATCH` when present.
- `list_owner_repos(owner) -> Vec<RepoRef>` — `GET /orgs/{org}/repos`, fall
  back to `GET /users/{username}/repos` on `404`. Follows `Link: rel="next"`
  pagination until the full listing is drained.
- `fetch_and_upsert_repos(pool, connection, owner, client) -> (added, existing)`
  — upsert each owner's repos into `github_repos` (`INSERT` on missing rows,
  skip existing); never deletes. Shared by the manual fetch handler and the
  auto-fetch scheduler so both produce identical logs.
- `push_secret(conn, owner, repo, secret_name, plaintext)` /
  `push_variable(conn, owner, repo, name, value)` — orchestrate the above.

### Auto-fetch scheduler — `crates/manager/src/services/github_fetch_scheduler.rs`

Spawned from `main.rs` (60s tick) next to the cron scheduler:

- **Leader election**: reuses the shared `scheduler_lease` table on a dedicated
  row (`id = 2`), so the job-dispatch scheduler (`id = 1`) and the fetch
  scheduler never kick each other out, and only one manager instance runs
  auto-fetch at a time.
- **Due check**: each tick scans connections with `fetch_interval_minutes > 0`
  and non-empty `fetch_owners`; a connection is due when `last_fetch_at` is
  null or older than the interval (`connection_is_due` — a pure, unit-tested
  function).
- **Run**: for each due connection, fetch every configured owner via
  `fetch_and_upsert_repos`, then stamp `last_fetch_at = now` (even if an owner
  failed, so a permanently failing owner does not retry on every tick).
- **Logging** (`tracing::info`, `info` is the default level):
  - `GitHub fetch scheduler started (check interval: 60s, lease id: 2)`
  - `github-sync: auto fetch cycle due connections due=N`
  - `github-sync: auto fetch due connection=… owners=[…]`
  - `github-sync: auto fetch owner complete connection=… owner=… added=… existing=…`
  - `github-sync: auto fetch owner failed connection=… owner=…` (`error`)

### API — `crates/manager/src/api/github_sync.rs`

**Connections** (all `PlatformAdmin`):

```
GET    /api/v1/github-sync/connections        → [{id, name, api_base_url, has_token,
                                                  fetch_owners, fetch_interval_minutes,
                                                  last_fetch_at, updated_at}]
POST   /api/v1/github-sync/connections        → create {name, api_base_url?, token,
                                                  fetch_owners?, fetch_interval_minutes?}
PUT    /api/v1/github-sync/connections/{id}   → update {name?, api_base_url?, token?,
                                                  fetch_owners?|null, fetch_interval_minutes?|null}
DELETE /api/v1/github-sync/connections/{id}   → delete
```

Update semantics for the fetch fields: absent → keep current; `null` → clear
owners / disable the interval; a value → validate (interval must be a positive
integer) and persist. Owners are normalized to a comma-separated string on
write.

`api_base_url` is validated on create/update: it must be `https` (plain `http`
is only accepted for `localhost` / `127.0.0.1`, e.g. a local GHE or tests).
This, together with the client refusing to follow `Link: rel="next"`
pagination URLs whose host differs from the configured base, keeps the stored
PAT from ever being sent to an arbitrary scheme/host. The client also enforces
a 30s request timeout so a hung endpoint cannot stall the scheduler.

**Repos** (all `PlatformAdmin`; repos belong to a connection):

```
GET    /api/v1/github-sync/repos              → [{id, connection_id, connection_name, owner, repo, updated_at,
                                                 binds: [{kind: "secret"|"variable", id, name, last_status, last_op}]}]
POST   /api/v1/github-sync/repos              → create {connection_id, owner, repo}
PUT    /api/v1/github-sync/repos/{id}         → update {owner?, repo?}
DELETE /api/v1/github-sync/repos/{id}         → delete (cascades binds)
POST   /api/v1/github-sync/repos/fetch        → {connection_id, owner} pull the owner's repos
                                               (owner is required; 404 org → falls back
                                               to user). Returns {added, existing, errors}.
POST   /api/v1/github-sync/test               → {connection_id, owner?, repo?} connectivity check.
                                               With owner/repo → verify read access; with neither →
                                               validate the token alone via GET /user, returns
                                               {ok, login}.
```

**Secret binds** — list is `TeamView` on the secret's team (read-only status);
mutations (`add` / `patch` / `delete` / `push`) are **PlatformAdmin-only** (they push with
a connection PAT):

```
GET    /api/v1/secrets/{id}/github-binds                    → list (has_override; never override plaintext)  (TeamView)
POST   /api/v1/secrets/{id}/github-binds                    → add {repo_id, override_value?}  (PlatformAdmin)
PATCH  /api/v1/secrets/{id}/github-binds/{bind_id}          → {override_value: string|null}   (PlatformAdmin)
DELETE /api/v1/secrets/{id}/github-binds/{bind_id}          → unbind              (PlatformAdmin)
POST   /api/v1/secrets/{id}/github-binds/push               → push all binds (or ?bind_id=… for one)  (PlatformAdmin)
```

**Variable binds** (require `PlatformAdmin`, matching `platform_vars`):

```
GET    /api/v1/variables/{id}/github-binds                  → list (includes override_value)
POST   /api/v1/variables/{id}/github-binds                  → add {repo_id, override_value?}
PATCH  /api/v1/variables/{id}/github-binds/{bind_id}        → {override_value: string|null}
DELETE /api/v1/variables/{id}/github-binds/{bind_id}        → unbind
POST   /api/v1/variables/{id}/github-binds/push             → push all binds (or ?bind_id=…)
```

Push semantics:
- Secrets: fetch a **fresh** public key per push (GitHub rotates keys — never
  cache across pushes), seal the plaintext, `PUT` the secret.
- Variables: resolve `effective = bind.override_value ?? platform_vars.value`,
  then `POST`/`PATCH` that value under the Buz Variable **name**.
- Per-binding result `{bind_id, owner, repo, status: "ok"|"failed", error?}`;
  a failed binding does not block the rest.
- **Dashboard Push All** calls `?bind_id=…` **once per bind** (sequential), with
  a 30s per-request timeout, so a slow GitHub round-trip cannot abort the whole
  fan-out under the global 8s axios default. The dialog shows `Pushing N/M…`
  and highlights the bind in flight.
- On success the bind row's `last_status/last_pushed_at/last_error` is updated
  and `last_op` is set to `push`, so the UI can show state and whether the
  latest action was a bind, a push, or a failed unbind.
- Every push records an audit event (`github_sync.push`) with the target repo
  and the secret/var name — **values never enter logs**.
- Name validation before push: Buz names are already UPPER_SNAKE_CASE
  (compatible with GitHub), but reject names starting with `GITHUB_` or a digit
  with a clear error.

Bind semantics:
- Adding a bind only registers the target repo locally
  (`last_op='bind'`, `last_status='pending'`); nothing is sent to GitHub until
  a push.
- A failed add (repo id not found, or the secret/var is already bound) records
  an audit event (`secret.github_bind.add` / `variable.github_bind.add` with
  `status: "failed"` + reason) before the error is returned — the failure is
  traceable even though no bind row exists.

Unbind semantics — unbinding is **remote-first**:
- `DELETE` the value on GitHub Actions first (`DELETE
  /repos/{owner}/{repo}/actions/secrets/{name}` / `…/actions/variables/{name}`);
  the local bind row is only removed after the remote cleanup succeeds.
- The DELETE is idempotent: GitHub returning `204` or `404` (never pushed)
  both count as success, so unbinding a fresh, never-pushed bind still
  completes.
- If the remote cleanup fails (e.g. token lost its write permission), the API
  returns the error and the bind row is **kept** and marked
  `last_op='unbind'`, `last_status='failed'`,
  `last_error='unbind failed: <reason>'`, so the UI renders the row as
  **unbind failed** and the operator can fix the connection and retry.
- Every unbind records an audit event (`secret.github_bind.remove` /
  `variable.github_bind.remove`) and logs at each stage — never values:
  - `github-sync: remote secret deleted` / `remote variable deleted`
    (`info`, 204) or `… already absent, delete skipped` (`info`, 404) from
    the service layer.
  - `github-sync: secret unbind failed at remote cleanup, keeping local bind`
    (`warn`) before the error propagates.
  - `github-sync: secret unbound and removed from repo` (`info`) after the
    local row is deleted.

List enrichment — the existing resource list endpoints also return each item's
GitHub bind summary, so the Secrets / Variables pages can render bound repos
without an extra request:

```
GET /api/v1/secrets      → each row gains  "github_binds": [{id, repo_id, connection_name, owner, repo, last_pushed_at, last_status, last_error, last_op}]
GET /api/v1/variables    → each row gains  "github_binds": [ … same shape … ]
```

Implemented as one batched query (`WHERE secret_id IN (…)` joined to
`github_repos` / `github_connections`), not N+1. Binding names never leak
values — only the bind metadata above.

### RBAC & security

- Connections and repos are **PlatformAdmin-only** (the PAT is the blast
  radius; repo entries determine push targets).
- **Binding repo 的操作（绑定 / 解绑 / 推送）一律 PlatformAdmin-only**：
  `add_secret_bind` / `delete_secret_bind` / `push_secret_binds` 使用
  `PlatformAdmin` 提取器 fail-closed 校验——team maintainer 只能**查看**
  绑定状态，不能增删改。这避免团队密钥被推送到任意 admin 注册的 repo
  （推送使用该 connection 的平台 admin PAT）。
- Variables are platform-level (`platform_vars`), so their binds are
  PlatformAdmin-only.
- The PAT is never logged; debug-mode log output only contains names, repos,
  and statuses.
- GitHub error mapping: `404` → repo not found; `401` → invalid token;
  `403` → insufficient permission (suggest checking token scope / fine-grained
  permissions).

## Frontend design

### Entry

- `SecretsVariables.vue` gains a third tab **GitHub Sync** (adminOnly) hosting
  **connection management and repo management**.
- **Bindings & push live inline in the resource pages**:
  - `Secrets.vue` row action → GitHub icon opens a **GitHub Sync dialog** for
    that secret.
  - `Variables.vue` row action → same dialog for that variable.
- Both list pages also render a **GitHub Sync status column** with the bound
  repos and their push state (see below).

### GitHub Sync tab (admin-only)

Two management sections:

1. **Connections** — CRUD over connections (name / api_base_url / token, with a
   show/hide toggle for the token field). Optional **auto-fetch settings**:
   owners (comma or newline separated) + interval minutes. Lists `has_token`
   (never the token itself) and an **Auto-fetch** column showing `Off` or
   `{N}m · owners…`.
2. **Repositories** — CRUD over repos, plus **Fetch from GitHub**: pick a
   connection (and optional owner) to bulk-import its repos — upsert-only,
   never deletes. Each row shows its connection, with a **Test** button that
   calls the test endpoint. Owner/repo are optional: leaving them empty just
   validates the token via `GET /user` and reports the authenticated account;
   providing an owner (and optionally a repo) verifies read access to it.

Both tables follow the shared table-interaction rules below.

### Table interactions (all GitHub Sync tables)

Every data table in this feature — connections, repositories, and the bind
lists — reuses the dashboard's existing **`useClientSort`** /
**`useClientPagination`** composables and **`TablePagination`** component
(already used by `Secrets.vue` / `Variables.vue`):

- **Search** — client-side keyword filter across the row's searchable columns
  (name, owner, repo, connection name, status). Placeholder `Search …` in the
  filter bar.
- **Auto sorting** — clickable column headers with asc/desc toggle and a
  sort-direction indicator (same `↕` marker as the existing pages); a sensible
  default sort per table (e.g. repos by `owner`, then `repo`; connections by
  `name`; bind lists by `repo`).
- **Pagination** — page size + page controls via `TablePagination`, with the
  same `pageSize` options and `N of M` range label used elsewhere.
- Empty states show a short hint (e.g. "No repos registered yet — Fetch from
  GitHub or add one manually").

### Bind dialog (per secret / variable)

1. **Bindings list** — table of `{connection, owner/repo, last pushed, status,
   error}` with per-row **Push** and **Unbind** buttons, plus a **Push all**
   action. Same search / sort / pagination rules as above (searchable by
   repo/connection/status).
2. **Add binding** — select a repo from the **pre-registered repo dropdown**
   (grouped by connection, searchable). No hand-typing of `owner`/`repo`;
   duplicate binds are rejected. If the target repo is missing, the operator
   adds it first in the GitHub Sync tab.
3. Result feedback per row (ok / failed + error message); re-push overwrites.

### Bound-resource status on the Repositories table

`GitHubSync.vue`'s **Repositories** table shows a **Bound** column listing every
secret / variable bound to that repo (`GET /api/v1/github-sync/repos` returns a
`binds` array per repo — batch-loaded, never values):

- Each entry is a chip: an `S`/`V` kind prefix + the resource name, colored by
  `last_status` (amber `pending` / emerald `ok` / red `failed`), same legend as
  the bind dialog. Hover shows the state (e.g. `Secret MY_SECRET — pushed`).
- A repo with no binds shows a muted "—".
- Repo search also matches bound resource names.
- This is the repo-centric inverse of the secret/var pages' bound-repo chips.

### Bound-repo status on the Secrets / Variables list pages

Both `Secrets.vue` and `Variables.vue` get a **GitHub Sync column** rendering
the item's `github_binds` from the enriched list API. Each binding is shown as
a small **status chip**:

| last_status | Chip | Meaning |
|-------------|------|---------|
| `pending`   | amber dot + `owner/repo` | bound, **not pushed yet** |
| `stale`     | amber dot + `owner/repo` | **needs push** — local value changed since last successful push |
| `ok`        | emerald dot + `owner/repo` | pushed; hover shows last-pushed time |
| `failed` + `last_op='push'` | red dot + `owner/repo` | last push failed; hover shows error |
| `failed` + `last_op='unbind'` | red dot + `owner/repo` | **unbind failed** (remote cleanup failed, row kept); hover shows error |

- Multiple bindings render as a wrap of chips; empty column shows a muted "—".
- The chips are **clickable** and open the same per-resource GitHub Sync bind
  dialog (rows are also reachable via a GitHub icon in the row actions).
- An extra sortable summary key (e.g. `github_status` = failed/stale/pending/ok/
  none) lets the operator sort rows by sync state.
- A list filter **Needs push** shows rows whose binds are `pending`, `stale`, or
  `failed` (cross-resource bulk push remains out of scope).
- After saving a value that dirty-marks inherit binds, the UI offers
  **Push now** / **Later** (opens the bind dialog when chosen).
- Values are never rendered — only `owner/repo` + status metadata.

### Form conventions

- Repo entry: connection, `owner`, `repo` are required fields (red `*`), e.g.
  `new-org` / `app-1`. The Fetch button removes the need to type repo lists
  manually.
- Bind to a repo only after it exists in the target GitHub org — creation of
  the repo itself stays out of scope (user creates it manually, as in the
  original workflow).

## Testing

- Unit: sealed-box encryption against pinned libsodium vectors; GitHub error
  mapping; push orchestration with a mock HTTP server.
- Integration: simulate GitHub API responses (public key, repo lists, PUT/PATCH
  success + failure) and assert repo-fetch upsert behavior and bind state
  transitions (implemented in `crates/manager/src/api/tests/github_sync_tests.rs`
  with a local mock GitHub HTTP server).
- Frontend: vitest for the bind dialog state machine; manual e2e against a
  real throwaway repo.

## Rollout

- Phase 1 (this design): connections CRUD (incl. per-connection auto-fetch),
  repos CRUD + fetch, bind/unbind, push with per-repo state, platform-admin UI.
- Phase 2 (optional): org/environment-level secrets-vars, delete-sync, batch
  bind+push for a whole team's secrets, sync history.

## FAQ — GitHub PAT & permissions

Real-world failures we hit while building/shipping this feature, and how to
fix them.

### PAT requirements checklist

| Token type | Repository access | Permissions |
|------------|-------------------|-------------|
| Fine-grained PAT | **All repositories** (or the specific repos you push to) | `Actions` **Read and write**, `Secrets` **Read and write**, `Variables` **Read and write** |
| Classic PAT | `repo` scope | `repo` covers Actions secrets/variables write |

If the org enforces an SSO policy, the PAT must also be **authorized for the
org** (GitHub shows a red "needs authorization" badge on the token until you
click through).

### 403 — "Please make sure your request has a User-Agent header"

The GitHub REST API rejects requests without a `User-Agent`. Buz sends
`buz-manager/<version>` on every call, so this only bites ad-hoc debugging:

```bash
curl -H "Authorization: Bearer $PAT" -H "User-Agent: buz-debug" \
  https://api.github.com/user/orgs
```

### 403 — "…forbids access via a fine-grained personal access tokens if the token's lifetime is greater than 366 days"

An **organization policy** caps fine-grained PAT lifetime. Recreate the token
(or adjust it at https://github.com/settings/personal-access-tokens) to ≤ 366
days — Buz cannot bypass this; the org owner can relax the policy if needed.

### Private repos are not fetched / the repo list is incomplete

Work through in this order:

1. **Resource owner is wrong or missing.** For a fine-grained PAT the
   *Resource owner* is single-select: it must include the org whose repos you
   want (not just your personal account). If the org shows a red
   "needs authorization" state, click in and **Authorize** it.
2. **Permissions too narrow.** Confirm the checklist above — `Actions`,
   `Secrets`, `Variables` all **Read and write** and the target repos selected.
3. **Wrong account / PAT created by a non-owner.** If the org page shows only
   your personal account as resource owner, the org is simply not in the
   token's scope — regenerate the PAT (ideally with an org owner account).
4. **Policy blocks access** (lifetime / SSO / fine-grained restrictions) — the
   error text usually names the exact rule; act on it or ask the org admin.

A quick way to verify a PAT from the shell (private repo returns 200):

```bash
curl -s -o /dev/null -w "%{http_code}\n" \
  -H "Authorization: Bearer $PAT" -H "User-Agent: buz-debug" \
  https://api.github.com/repos/<org>/<repo>
```
