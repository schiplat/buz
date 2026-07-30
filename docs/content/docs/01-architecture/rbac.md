---
title: Users, Teams & RBAC
weight: 5
description: "Phase 2 authorization model — platform admins, teams, four team roles, and resource scoping"
---

## Status

**Implemented (Phase 2).** Multi-user login, teams, four team roles, required `team_id` on pipelines/secrets, optional host team, and membership-filtered list APIs.

Related: [Secrets Access Control](/docs/01-architecture/secrets-access-control/) (`owner_group` synced from team slug; runtime prefers `team_id`).

---

## Goals

- Multi-user login (replace config-only admin)
- Team-scoped isolation for Pipelines, Secrets, Jobs
- Optional Team affiliation for Hosts
- Four team roles with a clear permission matrix
- List APIs filter by membership from day one (no “login = see everything” interim)

Out of scope for Phase 2 (defer to Phase 3): SSO/OIDC, user API tokens, approval flows, audit UI, per-step secret ACL, cross-team grant tables.

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

Runners and Manager config remain **global infrastructure** — Platform Admin only.

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
| `platform_admin` | Full access: all teams, runners, user/team admin, unscoped hosts |
| `member` | Only resources in teams they belong to, constrained by team role |

### Team role matrix

| Capability | Owner | Maintainer | Developer | Viewer |
|------------|:-----:|:----------:|:---------:|:------:|
| View pipelines / jobs / logs | ✓ | ✓ | ✓ | ✓ |
| Trigger pipeline | ✓ | ✓ | ✓ | ✗ |
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

**Webhook / cron**: still fire without a user session; authorization is “pipeline exists and is active.” Job records store `trigger_by` as today (`webhook` / `cron` / username).

---

## Data Model

```text
users
  id TEXT PK
  username TEXT UNIQUE NOT NULL
  password_hash TEXT NOT NULL
  display_name TEXT
  is_platform_admin INTEGER NOT NULL DEFAULT 0
  is_active INTEGER NOT NULL DEFAULT 1
  created_at, updated_at

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
| JWT `role` | Extend shared enum: `admin` (platform_admin) \| `user` \| `runner` |
| Team list in JWT? | **No** — load `team_members` per request (short TTL cache optional) so membership changes apply immediately |
| Runner tokens | Unchanged (`Role::Runner`) |

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

## Secret Resolution (unchanged tiers, Team-backed)

1. **Group auto-access**: `secret.team_id == pipeline.team_id`
2. **Explicit binding**: `pipeline_secrets` (bind requires Maintainer+)
3. **YAML name reference**: still resolved at dispatch, but only among secrets the pipeline’s team may access (auto + bound)

Cross-team secret use: only via explicit binding **and** the binder must be allowed to read that secret (typically platform admin, or future Phase 3 grant). Phase 2 default: binding UI only lists secrets from the **same team** (+ admin override).

---

## API Surface

| Method | Path | Auth |
|--------|------|------|
| GET | `/api/v1/auth/me` | JWT |
| GET/POST | `/api/v1/users` | Platform admin |
| PUT/DELETE | `/api/v1/users/:id` | Platform admin |
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
- Personal / CI API tokens
- Audit log UI
- Per-step secret ACL
- Cross-team secret grants
- Approval gates on trigger
