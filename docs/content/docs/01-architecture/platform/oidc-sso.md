---
title: OIDC Single Sign-On
weight: 15
description: "Design — sign in through any OIDC provider; Buz keeps teams and roles as the authorization authority"
---

## Status

**Implemented**: Phase 1 (OIDC sign-in with JIT provisioning, account-level
authentication origins, ticket handoff) and **web-based configuration**
(the Settings → SSO page below — no manager.toml edit or restart required).
Local login is unchanged; group→workspace mapping and SCIM remain future
phases. This page is both the design record and the operator reference.

## Goals

- Sign in to Buz through **any standards-compliant OIDC provider** (Entra ID,
  Okta, Keycloak, Google Workspace, self-hosted IdPs). Buz speaks generic OIDC
  only — no provider-specific code, configuration, or API calls.
- **Keep local username/password login** unchanged: existing deployments keep
  working; a local platform administrator remains the break-glass path when the
  provider is down.
- **Keep Buz's authorization model unchanged**: teams, the four team roles
  (`viewer < developer < maintainer < owner`), and platform-admin flags stay
  the single authorization authority. The IdP answers *who you are*; Buz
  answers *what you may do*.

## Non-goals

- No organization-tree-driven authorization. The `groups` claim is a flat list
  of direct group names with no departments, ancestors, or stable IDs; it is
  stored for display and later mapping, never used as a permission source on
  its own.
- No automatic linking of an OIDC identity to an existing local account by
  email or username (email is changeable; silent matching is an
  account-takeover vector).
- No two account systems. There is one `users` table; OIDC users are ordinary
  rows that authenticate differently (see the account model below).
- No SCIM provisioning in the first phase (tracked as a follow-up, see
  [Phasing](#phasing)).

## Account model

One authorization subject, two mutually exclusive authentication origins:

```
users.auth_source            TEXT    'local' (default) | 'oidc'
users.password_login_disabled BIGINT  0 (default) | 1
```

- **`local` accounts** authenticate with username/password exactly as today and
  cannot use OIDC sign-in.
- **`oidc` accounts** are rows created for OIDC users (no local password,
  `password_login_disabled = 1`) and can only sign in through the provider.
- The stable identity key is `(provider, subject)` — the IdP's `sub` — never
  email or display name. `email` / `name` claims are display-only attributes.
- A login attempt against the wrong origin fails with the same generic
  "invalid credentials" error as any other login failure; the response does not
  reveal which origins an account has.
- At least one **local platform administrator** should be preserved per
  instance as break-glass. Buz refuses to disable or demote the last active
  platform admin (existing invariant) and refuses identity changes that would
  leave an account with no way to sign in.

Every OIDC user is a full Buz user for authorization purposes: team
memberships, resource ownership, audit attribution, and API tokens all key on
the same `users.id`.

## Data model

Three tables (SQLite migration 4, Postgres migration 3) plus the two user
columns above:

```sql
-- One external identity per (provider, subject); binds to exactly one user.
CREATE TABLE external_identities (
    id           TEXT PRIMARY KEY,
    provider     TEXT NOT NULL,          -- 'oidc' (+ issuer identity below)
    issuer       TEXT NOT NULL,          -- bound issuer at link time (audited)
    subject      TEXT NOT NULL,          -- IdP `sub`
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    email        TEXT NOT NULL DEFAULT '',
    display_name TEXT NOT NULL DEFAULT '',
    groups_json  TEXT NOT NULL DEFAULT '[]',  -- display/audit only
    created_at   TEXT NOT NULL DEFAULT datetime(),
    last_login_at TEXT,
    UNIQUE (provider, subject),
    UNIQUE (provider, user_id)           -- one identity per user per provider
);

-- One outstanding authorization flow (state/nonce/PKCE are secrets at rest).
CREATE TABLE sso_flows (
    id            TEXT PRIMARY KEY,
    state_hash    TEXT NOT NULL UNIQUE,  -- SHA-256; plaintext state never stored
    nonce         TEXT NOT NULL,
    code_verifier TEXT NOT NULL,
    redirect_to   TEXT NOT NULL DEFAULT '/dashboard',  -- local path only
    expires_at    TEXT NOT NULL,         -- short TTL (10 min)
    consumed_at   TEXT,
    created_at    TEXT NOT NULL DEFAULT datetime()
);

-- One short-lived single-use ticket handing the browser session to the SPA.
CREATE TABLE sso_tickets (
    ticket_hash TEXT PRIMARY KEY,        -- SHA-256; plaintext never stored
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at  TEXT NOT NULL,           -- very short TTL (60 s)
    consumed_at TEXT,
    created_at  TEXT NOT NULL DEFAULT datetime()
);
```

Flow and ticket rows are consumed atomically (single use) and cleaned up by a
retention sweep alongside the existing debug-log cleanup jobs.

## Sign-in flow

```
SPA login page "Continue with SSO"
  → GET /api/v1/auth/oidc/start        (creates sso_flow, 302 to IdP)
  → IdP /oauth/authorize (PKCE S256, state, nonce)
  → user authenticates (+ MFA per IdP policy) and consents
  → GET /api/v1/auth/oidc/callback?code&state
       ├ atomically consume state
       ├ exchange code (+ verifier) at the token endpoint
       ├ validate id_token: RS256, iss, aud, exp/iat (±60 s skew), nonce
       ├ resolve (provider, subject) → user, or JIT-provision
       └ set HttpOnly ticket cookie, 303 → SPA /auth/sso/callback
  → POST /api/v1/auth/oidc/exchange    (consume ticket → LoginResponse)
  → SPA stores the Buz JWT exactly as after a local login
```

- The redirect parameter accepted by `/start` is restricted to same-site
  relative paths (`/…`, no `//`, no scheme) — no open redirect.
- The id_token, access token, code, verifier, and client secret are never
  logged, stored beyond the flow's lifetime, or sent to the browser. The
  browser only ever holds the Buz JWT.
- Local username/password login and `/setup` are untouched.

### Account resolution rules

1. `(provider, subject)` already bound → load the user; refuse when
   `is_active = 0`.
2. Unbound and `auto_provision = true` (default) → create a plain user
   (`auth_source='oidc'`, no teams, no admin flags; username derived from the
   `name`/`email` local-part, deconflicted with numeric suffixes), bind, audit.
3. Unbound and `auto_provision = false` → refuse with a message directing the
   user to an administrator.
4. Bound to a disabled user → refuse.
5. Converting an existing local account to OIDC sign-in is a deliberate,
   separate flow (Phase 1.5): the user proves local possession once, completes
   one OIDC round trip, then `auth_source` flips and local password login is
   disabled. Team memberships, audit history, and resource ownership are
   preserved because `users.id` does not change.

## Configuration

**Recommended: the web UI.** Settings → **SSO** (platform admin) edits every
field — issuer, client id/secret, redirect, scopes, auto-provision, enable —
with a built-in "Test connection" discovery probe. Saved settings live in the
database (`oidc_settings`), **override the file**, and apply immediately; the
client secret is stored encrypted at rest and never echoed back. Deleting the
override (Revert to manager.toml) falls back to the file seed.

`[auth.oidc]` in `manager.toml` remains the deployment-time seed (and the
IaC path); once an administrator saves from the web, the database is
authoritative:

```toml
[auth.oidc]
enabled = false
issuer = ""                       # https origin, no trailing slash
client_id = ""
client_secret = ""                # or client_secret_file / BUZ_OIDC_CLIENT_SECRET
redirect_uri = ""                 # absolute, must equal the IdP registration
scopes = ["openid", "profile", "email"]   # groups added when mapping lands
auto_provision = true
```

The callback URL derives from `server.public_base_url`
(`{public_base_url}/api/v1/auth/oidc/callback`); both the file seed and the
web form accept an explicit override for unusual proxy layouts.

Startup validation (when `enabled = true`): https issuer (plain http allowed
for localhost development), no trailing slash, absolute `redirect_uri`,
`openid` in scopes, non-empty credentials. A **misconfigured SSO disables only
the SSO endpoints** (reported through `/auth/providers` and the log) — it never
prevents Buz from starting, because local login remains fully functional.

`GET /api/v1/auth/oidc/providers` (public) reports whether SSO is enabled so
the login page can render the button conditionally.

## Security invariants

| Concern | Rule |
|---|---|
| Identity key | `(provider, subject)` from validated id_token; never email/username matching |
| Token exchange | Confidential client only; client_secret stays server-side; PKCE S256 always |
| Replay | `state` single-use + hashed at rest; `nonce` echoed in id_token and verified |
| Clock | ±60 s skew on `exp`/`iat`; JWKS cached with `kid` rotation support |
| Session handoff | 60 s single-use ticket in an `HttpOnly` cookie; `Cache-Control: no-store` on exchange |
| Account takeover | No silent linking; conversion requires proving local possession; disabled users refused |
| Failure mode | IdP down / misconfigured → SSO fail closed, local login unaffected; JWKS fetch failures refuse sign-in, never degrade local auth |
| Least privilege | JIT users get no teams, no owner role, no platform-admin flags — ever |
| Hygiene | No secrets or tokens in logs/argv; flows/tickets hashed, single-use, TTL'd, swept |

## Known limitations (documented, accepted)

- Revocation of team membership through IdP groups is **not** in scope: Buz
  does not read IdP groups for permissions (see
  [Non-goals](#non-goals) and [Phasing](#phasing)).
- Buz JWTs are stateless: disabling a user blocks every subsequent request
  (`is_active` is checked per request), but an already-issued JWT stays valid
  until expiry. The standard TTL applies; RP-initiated logout is Phase 1.5.
- Group claims are stored but inert in Phase 1.

## Phasing

- **Phase 1 (this design)** — OIDC sign-in, JIT provisioning, account
  resolution rules, break-glass local login, audit events
  (`auth.oidc_login`, `auth.oidc_provision`), configuration, docs, tests
  (mock IdP; SQLite and Postgres fixtures).
- **Phase 1.5** — local→OIDC account conversion; group → workspace role links
  synced at login (explicit admin-configured links, `source`-separated
  memberships, additive by default, owner/platform-admin never auto-granted);
  RP-initiated logout; admin identity management views.
- **Phase 2** — SCIM 2.0 server (Users/Groups) for real-time provisioning and
  deprovisioning by IdP/IGA systems; multiple simultaneous providers;
  session revocation (`session_epoch`).

## Alternatives considered

- **Provider-specific integration** (IdP admin APIs, provider group UUIDs,
  provider-issued sessions) — rejected: couples Buz to one product, breaks the
  "works with any OIDC provider" contract, and turns a claims protocol into a
  coupling protocol.
- **Email-based account matching** — rejected: email is changeable and
  verified-ness is provider-dependent; silent matching is an account-takeover
  vector.
- **Two account systems (local + SSO tables/RBAC)** — rejected: every
  permission check, team membership, audit row, and resource owner would fork;
  one person would hold two identities with divergent permissions.
- **Organization tree as the authorization source** — rejected: the OIDC
  `groups` claim carries neither the tree nor stable IDs, and login-time claims
  cannot revoke access. Identity providers answer identity; resource
  authorization stays with Buz.
