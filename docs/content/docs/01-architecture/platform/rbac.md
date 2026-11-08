---
title: Users, Teams & RBAC
weight: 10
description: "Phase 2 authorization model — platform admins, teams, four team roles, and resource scoping"
aliases:
  - /docs/01-architecture/rbac/
---

{{< callout type="note" >}}**Design record — implemented** (Phase 2 + Phase 3 auth items).{{< /callout >}}

## Status

**Implemented (Phase 2 + Phase 3 auth items).** Multi-user login, teams, four team roles, required `team_id` on pipelines/secrets, optional host team, membership-filtered list APIs, platform-admin API tokens, and tiered audit-log visibility (super admin vs regular admin).

Related: [Secrets Access Control](/docs/01-architecture/platform/secrets-access-control/) (`owner_group` synced from team slug; runtime prefers `team_id`).

---

## Goals

- Multi-user login (replace config-only admin)
- Team-scoped isolation for Pipelines, Secrets, Jobs
- Optional Team affiliation for Hosts
- Four team roles with a clear permission matrix
- List APIs filter by membership from day one (no “login = see everything” interim)

Out of scope for Phase 2 (defer to Phase 3): SSO/OIDC, user API tokens, per-step secret ACL, cross-team grant tables.

Environment approval gates (方案 B) use team role thresholds (`min_trigger_role` / `min_approve_role`) with append-only `job_approvals` — see [Environment Approval Gates](../environment-approvals/).

Platform-admin **Audit logs** UI/API (mutation before/after with redaction) is available — see [Admin Audit Logs](/docs/01-architecture/platform/admin-audit-logs/).

---

## Tenancy Model

**Single Manager instance · multi-team** (not multi-org SaaS).

```
Manager (= one deployment)
 ├── Platform Admin(s)
 ├── Team: platform
 ├── Team: backend
 └── Team: prod
      ├── members (Owner / Maintainer / Developer / Viewer)
      ├── pipelines + secrets (required team_id)
      └── hosts (optional team_id)
```

Runner management remains **platform-admin-only** global infrastructure. Job **dispatch** can optionally be scoped per team: each Runner has a nullable `team_id` — `NULL` is the explicit **Shared** mode (jobs from every team, existing runners keep it after upgrade), while a bound Runner only receives jobs whose trigger-time team matches. Team binding limits Manager scheduling only; it is **not** an OS-level sandbox — jobs on one Runner still share the same OS user and host resources.

---

## Confirmed Product Rules

| # | Decision |
|---|----------|
| 1 | **Hosts**: optional `team_id`. `NULL` = shared / admin-managed; non-null = team-scoped |
| 2 | **Pipelines & Secrets**: `team_id` **required**. Unscoped resources are forbidden |
| 3 | **Team roles**: Owner, Maintainer, Developer, Viewer (four tiers) |
| 4 | **Developer**: may edit Pipelines (and pipeline Variables); **must not** manage Secrets |
| 5 | **Delivery**: ship membership filtering with teams (2b) — no multi-user-without-isolation interim |
| 6 | **Team picker UI**: show Team select only for **platform admins** or users in **multiple teams**. Single-team members auto-bind their sole membership (no Team field in create/edit forms) |
| 7 | **Team switcher**: same visibility as rule 6; filters Pipelines / Secrets / Jobs / Hosts lists. Default = **All my teams**. Hosts filter includes shared (`team_id` null) hosts when a team is selected |

---

## Two-Layer Authorization

### Platform role

| Role | Capability |
|------|------------|
| `super_admin` | Everything a platform admin can do, plus: view **all** audit logs and list/revoke **any** admin's API tokens; grant/revoke the super-admin flag |
| `platform_admin` | Full access: all teams, runners, user/team admin, unscoped hosts, own API tokens |
| `member` | Only resources in teams they belong to, constrained by team role |

`is_super_admin` is a separate boolean on `users`. The first admin created via `/setup` is a super admin. Regular platform admins see a filtered audit log (only their own actions) and only their own API tokens. See [Admin Audit Logs](/docs/01-architecture/platform/admin-audit-logs/).

### Team role matrix

| Capability | Owner | Maintainer | Developer | Viewer |
|------------|:-----:|:----------:|:---------:|:------:|
| View pipelines / jobs / logs | ✓ | ✓ | ✓ | ✓ |
| Trigger pipeline | ✓ | ✓ | ✓ | ✗ |
| Trigger **protected** Environment | role ≥ env `min_trigger_role` (default developer) | same | same | ✗ |
| Approve / reject protected deploy | role ≥ env `min_approve_role` (default maintainer); self-approve forbidden by default | same | same* | ✗ |
| Create / edit pipeline + variables | ✓ | ✓ | ✓ | ✗ |
| Delete pipeline | ✓ | ✓ | ✗ | ✗ |
| View secret metadata (name, team) | ✓ | ✓ | ✓ | ✓ |
| Create / update / delete secret | ✓ | ✓ | ✗ | ✗ |
| Bind secrets to pipeline | ✓ | ✓ | ✗ | ✗ |
| Manage team-scoped hosts | ✓ | ✓ | ✗ | ✗ |
| Use / reference hosts in SSH steps | ✓ | ✓ | ✓ | ✗* |
| Manage team members | ✓ | ✗ | ✗ | ✗ |
| Delete team | ✓ | ✗ | ✗ | ✗ |

\*Viewer may see host **names** used by pipelines they can view, but not credential fields (same as today: credentials never returned to UI).

\*Approve/reject: Developer may approve only when the Environment sets `min_approve_role=developer`. Default is Maintainer+. See [Environment Approval Gates](../environment-approvals/).

**Webhook / cron**: still fire without a user session for unprotected paths; **protected Environments reject automated triggers**. Job records store `trigger_by` as today (`webhook` / `cron` / username).

---

## Data Model

```text
users
  id TEXT PK
  username TEXT UNIQUE NOT NULL
  password_hash TEXT NOT NULL
  display_name TEXT
  is_platform_admin INTEGER NOT NULL DEFAULT 0
  is_super_admin INTEGER NOT NULL DEFAULT 0
  is_active INTEGER NOT NULL DEFAULT 1
  created_at, updated_at

api_tokens
  id TEXT PK
  user_id TEXT NOT NULL → users(id) ON DELETE CASCADE
  name TEXT NOT NULL
  token_hash TEXT NOT NULL          -- SHA-256, plaintext never stored
  token_prefix TEXT NOT NULL        -- display e.g. "buz_a1b2c3…"
  created_at, last_used_at, revoked_at

teams
  id TEXT PK
  name TEXT NOT NULL
  slug TEXT UNIQUE NOT NULL          -- replaces free-form owner_group
  description TEXT DEFAULT ''
  created_at, updated_at

team_members
  team_id TEXT NOT NULL → teams(id) ON DELETE CASCADE
  user_id TEXT NOT NULL → users(id) ON DELETE CASCADE
  role TEXT NOT NULL                 -- owner | maintainer | developer | viewer
  PRIMARY KEY (team_id, user_id)

-- Resource ownership
pipelines.team_id  TEXT NOT NULL → teams(id)   -- after migration; required
secrets.team_id    TEXT NOT NULL → teams(id)
hosts.team_id      TEXT NULL → teams(id)       -- optional

-- Transition: keep owner_group TEXT until migration complete;
-- runtime prefers team_id match; may fall back to slug == owner_group.
```

### Migration from Phase 1 `owner_group`

1. Create `users` / `teams` / `team_members`.
2. If `users` is empty, create the first `platform_admin` via Web UI `/setup` (or `POST /auth/setup`).
3. For each distinct non-empty `owner_group` on pipelines/secrets: upsert `teams` with `slug = owner_group`.
4. Backfill `team_id`; fail closed on create/update if `team_id` missing.
5. UI: Owner Group text field → **Team** dropdown.
6. Deprecate writing free-form `owner_group`; keep column read-only or synced from `teams.slug` during transition.

Empty `owner_group` rows from Phase 1 must be assigned a team during upgrade (operator chooses default team or admin-only quarantine team).

---

## Authentication

| Concern | Design |
|---------|--------|
| Password | **Argon2id** PHC string in `users.password_hash` (not AES — that is only for Secrets/Hosts) |
| JWT `sub` | `user_id` (not username) |
| JWT `role` | Shared enum: `admin` (platform_admin) \| `user` \| `ops` (platform-ops, sandbox hosts only) \| `runner`. Authorization failures return `AppError::Forbidden` (HTTP 403, code 1003); authentication failures are 401 (`AuthFailed` 1001, expired/invalid token 1002). The super-admin tier (`is_super_admin`) gates cross-admin visibility: all API tokens, all audit events, granting the super flag. |
| Team list in JWT? | **No** — load `team_members` per request (short TTL cache optional) so membership changes apply immediately |
| Runner tokens | Unchanged (`Role::Runner`) |
| **API tokens** | `Authorization: Bearer buz_…` → SHA-256 lookup in `api_tokens`, then load the owning user plus the token's scope band. Tokens are **operations-plane credentials**: every scope is denied the identity plane (users, api-tokens, password change) and the governance plane (settings, plugin sources/pool) — those need a browser session. Scopes: `read` (default; safe methods only) and `ops` (read/write) for new tokens; legacy `full` (creator-equivalent operations-plane access) only on pre-scope rows until revoked. Stored hashed; plaintext shown once at creation |

Config: first platform admin is **not** created from toml. On empty `users`, open the Web UI `/setup` (or `POST /auth/setup`).

### Middleware

- Human routes use `AuthUser` / `PlatformAdmin`; runners use `RunnerClaims`.
- Helpers: `require_platform_admin()`, `require_team_perm(team_id, Perm::…)`.
- **List endpoints filter by visible `team_id`s**; do not return foreign-team rows then 403.

---

## Hosts Scoping

| `hosts.team_id` | Who can CRUD | Who can attach to SSH steps |
|-----------------|--------------|-----------------------------|
| `NULL` | Platform admin | Any developer+ in any team (reference by name/labels) |
| `<team>` | Owner / Maintainer of that team (+ platform admin) | Developer+ of that team (and admin) |

Credentials remain encrypted server-side; UI never receives plaintext (unchanged).

---

## Secret Resolution (Team-backed + `team_shared`)

1. **Team auto-access (opt-in)**: `secret.team_id == pipeline.team_id` **and** `secret.team_shared == true`
2. **Explicit binding**: `pipeline_secrets` / YAML `secrets:` (bind requires Maintainer+; same team only)
3. **Step name reference**: step `secrets:` or purpose slots resolved at dispatch only if `secret.team_id == pipeline.team_id`

New secrets default to `team_shared = false` (no auto-inject). See [Secrets Access Control](/docs/01-architecture/platform/secrets-access-control/).

Cross-team secret use: forbidden in Phase 2 (future Phase 3 grant). Binding UI lists secrets from the **same team** (+ admin override).

---

## API Surface

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/auth/me` | JWT |
| GET/POST | `/api/v1/users` | Platform admin |
| PUT/DELETE | `/api/v1/users/:id` | Platform admin |
| GET/POST | `/api/v1/api-tokens` | Platform admin (list: super admin sees all) |
| DELETE | `/api/v1/api-tokens/:id` | Platform admin (super admin: any; else own) |
| GET/POST | `/api/v1/teams` | JWT (create: platform admin) |
| GET/PUT/DELETE | `/api/v1/teams/:id` | Team role / owner |
| GET/PUT | `/api/v1/teams/:id/members` | Team view / owner |
| DELETE | `/api/v1/teams/:id/members/:user_id` | Team owner |

| Area | Change |
|------|--------|
| `POST /auth/login` | Validate against `users` table (Argon2id) |
| Pipelines / Secrets / Jobs | Filter + mutate by team perm; `team_id` required on create |
| Hosts | Filter: admin all; members see shared (`NULL`) + own teams |
| Runners | Platform admin only |
| Variables | Any user can list; create/update/delete platform admin |
| Environments | Any user can list (Trigger dropdown); create/update/delete + overrides + protection platform admin; approve/reject gated by env `min_approve_role` |

Error codes: keep `1002` auth, `1003` forbidden (insufficient permissions).

**Upgrade note:** Existing JWTs that used username as `sub` are invalid after upgrade — users must log in again. First admin is created via `/setup` when `users` is empty (not from config).

---

## Frontend

- Login unchanged UX; session stores user JWT + `/me` cache
- **Team switcher** (top bar): platform admins and multi-team members see **All my teams** | `<team>` filter. Selection persists in `localStorage` (`buz_active_team`)
- Filtered list pages: Pipelines, Secrets, Jobs, Hosts (Hosts also keep **shared** / unscoped hosts when a team is selected)
- Create forms prefer the active switcher team (`preferredTeamId`), then sole membership
- Hide Runners + Users admin from non–platform-admin
- Pipeline / Secret forms: required Team select only when `showTeamPicker` (admin or multi-team); single-team users auto-bind
- Host form: Team optional (“Shared (admin)”)

### Team switcher behavior

| Actor | Switcher visible? | Options |
|-------|:-----------------:|---------|
| Single-team member | No | N/A — API already membership-scoped |
| Multi-team member | Yes | All my teams + memberships from `/me` |
| Platform admin | Yes | All my teams + all teams from `GET /teams` |

Filtering is **client-side** on top of membership-filtered list APIs (no extra query param). Jobs list includes `team_id` (from the pipeline) for the filter.

---

## Delivery Phases (implementation order)

| Step | Status |
|------|--------|
| **2a** Schema + bootstrap admin + DB login | Done |
| **2b** Teams, required `team_id`, list filtering, optional host team | Done |
| **2c** Four-role matrix on write/trigger/secret APIs | Done |

There is **no** production interim where multiple users share one flat admin view.

---

## Phase 3 (explicitly later)

- OIDC / SSO
- Approval gates on trigger
- Per-step secret ACL
- Cross-team secret grants

**Completed during Phase 3 iteration:**
- Personal / CI API tokens (UI + hashed storage + audit) — see [API Reference](/docs/01-architecture/platform/api-reference/)
- Audit log UI with tiered visibility (super admin vs regular admin) — see [Admin Audit Logs](/docs/01-architecture/platform/admin-audit-logs/)
