---
title: Team switcher & Docker e2e delivery
weight: 40
description: "Implementation notes for multi-team list filtering and Docker engine acceptance e2e"
---

## Scope

Completed in one delivery pass:

1. **RBAC UX** — top-bar team switcher / All my teams filter so multi-team accounts are usable day-to-day
2. **Docker MVP acceptance** — real-daemon ignored e2e tests + docs acceptance matrix checked off

---

## 1. Team switcher / filter

### Problem

List APIs already membership-filter (`team_in_clause`). Multi-team users (and platform admins) still saw **all** visible teams mixed together, with no way to focus one team while browsing Pipelines / Secrets / Jobs / Hosts.

### Approach (client-side scope)

| Piece | Change |
|-------|--------|
| `frontend/src/utils/teamFilter.ts` | Pure `matchesTeamFilter(resourceTeamId, activeTeamId, { includeShared? })` |
| `frontend/src/stores/auth.ts` | `activeTeamId` (persisted), `showTeamSwitcher`, `preferredTeamId`, `matchesActiveTeam` |
| `TeamSwitcher.vue` + `TopNav` | Select: All my teams \| team… (admin loads `GET /teams`) |
| Pipelines / Secrets / Jobs / Hosts | `filtered*` computed lists; empty copy distinguishes “none at all” vs “none in team” |
| `GET /jobs` | Response includes `team_id` from joined pipeline for filtering |
| Create forms | Default team = `preferredTeamId` (active switcher, else sole membership) |

### Why not a query param?

Membership filtering already happens on the server. An optional `?team_id=` would duplicate ACL logic and require every list handler to change. Client filter is enough for MVP and keeps authorization on the server.

### Docs

See [Users, Teams & RBAC](/docs/01-architecture/platform/rbac/) — product rule **#7** and Frontend section.

---

## 2. Docker acceptance e2e

### Problem

Unit tests covered argv / preflight / YAML validation, but C1–C8 execution semantics still pointed at “manual e2e”.

### Approach

| Piece | Change |
|-------|--------|
| `crates/runner/src/engine/docker_e2e_tests.rs` | `#[ignore]` tokio tests against a real daemon |
| `scripts/docker-engine-e2e.sh` + `make test-docker-e2e` | Pull `alpine:3.20`, set `BUZ_DOCKER_E2E=1`, run ignored tests |
| `example/pipelines/docker-engine-smoke.yml` | Workspace write + step env for product smoke |
| `docker-engine.md` | Acceptance matrix with ✅ / ⚠ status |

Default `cargo test --all` does **not** run ignored tests (CI without Docker stays green).

### Commands

```bash
make test-docker-e2e
# or
BUZ_DOCKER_E2E=1 cargo test -p runner docker_e2e -- --ignored --nocapture
```

Manual Manager+Runner smoke remains documented for D1 (UI log streaming).
