---
title: OIDC Single Sign-On
weight: 15
description: "Design — sign in through any OIDC provider; Buz keeps teams and roles as the authorization authority"
---

## Status

**Implemented**: Phase 1 (OIDC sign-in with JIT provisioning, account-level
authentication origins, ticket handoff), **web-based configuration**
(the Settings → SSO page below — no manager.toml edit or restart required),
and **provider-initiated session revocation** (back-channel logout: disabling
or logging out a subject at the IdP kills that user's Buz JWTs within
seconds). Local login is unchanged; account conversion and group→workspace
mapping remain future phases. The implementation is **end-to-end verified
against Signet**; for the operator walkthrough see
[SSO in Ten Minutes with Signet](/docs/00-quick-start/signet-sso/). This page
is both the design record and the operator reference.

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
- **Buz never implements a SCIM server.** Provisioning is the identity
  provider's job (the companion IdP implements SCIM 2.0 inbound); an RP that
  accepts pushed accounts would gain a second provisioning path beside OIDC,
  duplicating account state and bypassing the sign-in record. The signal Buz
  needs from the IdP — "this subject is gone" — is standard OIDC back-channel
  logout, not SCIM.

## Account model

One authorization subject, two mutually exclusive authentication origins:

```
users.auth_source            TEXT    'local' (default) | 'oidc'
users.password_login_disabled BIGINT  0 (default) | 1
users.sessions_revoked_epoch BIGINT   0 (default) | Unix second of the last
                                      IdP-initiated account-wide revocation
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

Three sign-in tables (SQLite migration 4, Postgres migration 3), the approval
queue (`pending_identities`, SQLite migration 7 / Postgres migration 6), and
the user columns above (`sessions_revoked_epoch` is SQLite migration 6 /
Postgres migration 5):

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

-- Identities refused at sign-in (auto_provision = off), awaiting an admin.
CREATE TABLE pending_identities (
    id            TEXT PRIMARY KEY,
    provider      TEXT NOT NULL,
    issuer        TEXT NOT NULL DEFAULT '',
    subject       TEXT NOT NULL,
    email         TEXT NOT NULL DEFAULT '',
    display_name  TEXT NOT NULL DEFAULT '',
    groups_json   TEXT NOT NULL DEFAULT '[]',
    first_seen_at TEXT NOT NULL DEFAULT datetime(),
    last_seen_at  TEXT NOT NULL DEFAULT datetime(),
    attempts      BIGINT NOT NULL DEFAULT 1,
    UNIQUE (provider, subject)
);
```

Flow and ticket rows are consumed atomically (single use) and cleaned up by a
retention sweep alongside the existing debug-log cleanup jobs. Pending
identity rows are display/queue data: they never influence sign-in
decisions, and dismissing one is not a blocklist (the next attempt
re-records it).

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
   Until a team + role is granted, the SPA keeps the account on a **waiting
   page** (no shell or menu); the page re-checks `/me`, so access appears
   without signing in again. Platform admins may sign in without a team.
3. Unbound and `auto_provision = false` → refuse with a message directing the
   user to an administrator, and **record the refused identity in the
   approval queue** (`pending_identities`, refreshed on every refused
   attempt). A platform admin resolves it from the **Users & teams**
   page (`/users`, pending-approval queue at the top): bind the
   identity to an existing account, create a plain SSO user for it, or
   dismiss it (a later attempt re-records it). Binding to an existing
   account keeps its local password unless the admin explicitly disables it
   — and the last active local platform admin can never lose password login
   through this path. Every approval/dismissal is audited
   (`auth.oidc_identity_approve` / `auth.oidc_identity_reject`).
4. Bound to a disabled user → refuse.
5. Converting an existing local account to OIDC sign-in by the user (proving
   local possession, one OIDC round trip) remains a deliberate, signal-driven
   follow-up; the admin bind above already covers the migration path
   (`users.id` unchanged either way).

## Session revocation (back-channel logout)

Buz JWTs are stateless, and `is_active` is re-checked on every request — so a
disable **in Buz** takes effect immediately. A disable or logout **at the
IdP** is different: the provider knows, Buz does not. The standard answer is
[OpenID Connect Back-Channel Logout 1.0](https://openid.net/specs/openid-connect-backchannel-1_0.html),
and the companion IdP already sends it.

```
IdP: admin disables the user / user logs out / password reset / MFA reset /
     email change / SCIM disable
  → POST {public_base_url}/api/v1/auth/oidc/backchannel-logout
       logout_token=<signed JWT>            (form-encoded)
  → Buz verifies RS256 against the provider JWKS, iss, aud, exp (when
    present), the logout event member, iat freshness (<= 300 s), and that no
    nonce is carried
  → maps `sub` -> external_identities -> users
  → users.sessions_revoked_epoch = max(current, now)
  → every later request carrying a JWT whose iat <= that instant is refused
```

- **Covers** every account-wide revocation the provider reports — IdP-side
  disable, logout, password/MFA reset, email change, SCIM-driven
  deactivation.
- **Does not cover API tokens** (`buz_…`): those are Buz-owned credentials with
  their own lifecycle, revoked from Buz rather than by the IdP. Disabling the
  user (`is_active = 0`) still blocks them.
- The endpoint is public: the signed logout token is the credential. Unknown
  subjects answer `200` (no account enumeration); `sid`-only tokens answer
  `501` (Buz keeps no session rows, so a provider session id cannot be mapped
  to anything revoke-able — and a `200` would record a success that did not
  happen); malformed or forged tokens answer `400`. Revocation is idempotent
  and audited (`auth.oidc_backchannel_logout`, system actor). Register Buz
  with subject-based logout only; a token carrying **both** `sub` and `sid`
  is processed by its subject and revokes every Buz session of that account —
  the strongest thing Buz can do, and more than the one session the provider
  named.
- The 300 s freshness window bounds replay; a replayed token inside the window
  re-stamps the same instant and changes nothing.

Register the URL at the provider — the Settings → SSO page shows the exact
value derived from `server.public_base_url`, next to the callback URL.

## Sign-out (RP-initiated logout)

Destroying the Buz JWT is client-side (stateless tokens); the question is
what happens to the **provider session**. Buz follows the standard
RP-initiated logout shape — a **top-level GET** with the `id_token_hint`:

```
Dashboard "Sign out"
  → clears the local JWT, navigates to
     GET /api/v1/auth/oidc/logout        (no-store)
       ├ SSO off            → 303 to the SPA login page
       ├ hint cookie present (unexpired id_token from the last sign-in)
       │    → 303 to {end_session_endpoint}?id_token_hint=…&client_id=…
       │      &post_logout_redirect_uri=…
       │      (a top-level GET, so the provider's SameSite=Lax session
       │      cookie rides along; a cross-site form POST would not)
       │    → the provider ends its session, fires its back-channel logout
       │      (revoking the Buz JWTs of other devices too), and returns the
       │      browser to the SPA landing /auth/sso/logged-out
       └ no hint / expired hint / no advertised endpoint
                            → 303 straight to the landing page
```

- The **hint cookie** (`buz_oidc_hint`) is set at sign-in: HttpOnly,
  `SameSite=Lax`, scoped to `/api/v1/auth/oidc`, expiring with the id_token
  itself. It holds the raw `id_token` and is used for nothing else — losing
  it only degrades sign-out to local-only, never weakens sign-in. Sign-out
  always clears it.
- Register the landing page
  (`{public_base_url}/auth/sso/logged-out`, shown on the Settings → SSO
  page) as the client's `post_logout_redirect_uri`; without it the provider
  typically still ends the session but keeps the browser on its own page.
- Logout-CSRF is the provider's concern (worst case: someone ends your IdP
  session); Buz adds `Cache-Control: no-store` and nothing else. The hint
  expiry check is Buz's own fallback UX — the provider re-validates the
  hint for real, and an expired hint degrades to a local sign-out instead
  of stranding the user on a provider error page.

Token-endpoint client authentication is negotiated from discovery:
`client_secret_post` when the provider declares it (the common case),
`Authorization: Basic` otherwise — providers that accept both work with
either. `client_secret_post` stays the fallback when discovery is silent.
The SSO settings page offers an explicit override (`auto` /
`client_secret_basic` / `client_secret_post`) for providers whose discovery
document is wrong or silent; `auto` keeps the negotiation above.

## Configuration

**The database is the only source.** Settings → **SSO** (platform admin) edits
every field — issuer, client id/secret, redirect, scopes, sign-in policy,
token-endpoint auth, groups claim, enable — with a built-in "Test connection"
discovery probe. Saved settings apply immediately (no restart); the client
secret is stored encrypted at rest and never echoed back. Clearing the
configuration (a ghost button in the status area) turns SSO off until an
administrator saves again — local login keeps working throughout.

**Groups claim.** Buz reads one id_token claim (default `groups`) and mirrors
it into `external_identities.groups_json` for display and audit. The claim
name is configurable for providers that use a different one, and a provider
sending a single string instead of an array is accepted. It carries **no
authorization** — no role rules, no team mapping.

**Access policy.** The same page's sidebar exposes two global settings
(`platform_settings` overrides, manager.toml defaults):

- **Session lifetime (hours)** — the user-JWT `exp` applied at mint (default
  24, `[auth] jwt_ttl_hours`). It applies to local and SSO sign-ins alike and
  affects only tokens issued **after** the change.
- **Idle timeout (minutes)** — the SPA signs the browser out after that much
  inactivity (0 = off). This is client-side UX, **not** a revocation: the
  issued JWT stays valid until its session lifetime ends. Activity is shared
  across tabs through `localStorage`.

**Recent SSO events.** A read-only card lists the newest SSO audit events
(`auth.oidc_login`, `auth.oidc_provision`, identity approve/reject,
back-channel logout), platform-wide for platform administrators. Local
password logins (`auth.login`) stay behind the stricter `/audit-logs` actor
scoping and never appear here.

**Legacy file seed — imported once, then ignored.** A deployment configured
through `[auth.oidc]` in `manager.toml` keeps working on upgrade: at startup,
a valid file section with no database row is **imported into the row** (the
secret re-encrypted at rest, the row stamped `file-import`, the log announces
"`[auth.oidc]` imported into the database — the section is now ignored").
Afterwards the file section is dead text and should be removed from
`manager.toml`. The section is no longer a supported configuration path; new
deployments configure SSO in the web UI.

The callback URL derives from `server.public_base_url`
(`{public_base_url}/api/v1/auth/oidc/callback`); the web form accepts an
explicit override for unusual proxy layouts.

Startup validation (when the row enables SSO): https issuer (plain http
allowed for localhost development), no trailing slash, absolute
`redirect_uri`, `openid` in scopes, non-empty credentials. A
**misconfigured SSO disables only the SSO endpoints** (reported through
`/auth/providers` and the log) — it never prevents Buz from starting, because
local login remains fully functional.

`GET /api/v1/auth/oidc/providers` (public) reports whether SSO is enabled so
the login page can render the button conditionally.

## Security invariants

| Concern | Rule |
|---|---|
| Identity key | `(provider, subject)` from validated id_token; never email/username matching |
| Token exchange | Confidential client only; client_secret stays server-side; PKCE S256 always; client auth = `client_secret_basic` by default (RFC 6749 §2.3.1), form credentials only when the provider declares `client_secret_post` and not Basic — overridable per instance |
| Replay | `state` single-use + hashed at rest; `nonce` echoed in id_token and verified |
| Clock | ±60 s skew on `exp`/`iat`; JWKS cached with `kid` rotation support |
| Session handoff | 60 s single-use ticket in an `HttpOnly` cookie; `Cache-Control: no-store` on exchange |
| Revocation | IdP back-channel logout refuses every user JWT minted at or before the event; API tokens are Buz-owned and unaffected; invalid tokens fail closed |
| Account takeover | No silent linking; conversion requires proving local possession; disabled users refused |
| Failure mode | IdP down / misconfigured → SSO fail closed, local login unaffected; JWKS fetch failures refuse sign-in, never degrade local auth |
| Least privilege | JIT users get no teams, no owner role, no platform-admin flags — ever |
| Hygiene | No secrets or tokens in logs/argv; flows/tickets hashed, single-use, TTL'd, swept; the id_token is retained only as the logout hint (HttpOnly cookie, OIDC-path scoped, expires with the token) |

## Known limitations (documented, accepted)

- Revocation of team membership through IdP groups is **not** in scope: Buz
  does not read IdP groups for permissions (see
  [Non-goals](#non-goals) and [Phasing](#phasing)).
- Buz JWTs are stateless: disabling a user **in Buz** blocks every subsequent
  request (`is_active` is checked per request). A disable **at the IdP** is
  delivered by back-channel logout (above) and takes effect within seconds.
  Sign-out ends the provider session too (RP-initiated logout, above); when
  the provider does not advertise an end-session endpoint, only the Buz
  session ends.
- There is **no server-side session list** and no per-session revocation: Buz
  persists no session rows (a `sid` claim / session table was deliberately
  not added). The idle timeout is client-side only; the revocation primitives
  are the JWT TTL (~session lifetime) and the account-wide
  `sessions_revoked_epoch`. Consequences: an IdP-side logout ends **all** Buz
  sessions of that account at once (every device), and a `sid`-only logout
  token is refused with `501` (above) rather than silently ignored.
- Group claims are stored but inert in Phase 1.

## Phasing

- **Phase 1 (delivered)** — OIDC sign-in, JIT provisioning, account
  resolution rules, break-glass local login, audit events
  (`auth.oidc_login`, `auth.oidc_provision`), configuration, docs, tests
  (mock IdP; SQLite and Postgres fixtures).
- **Phase 1.5 (delivered)** — web configuration (DB > file seed, hot swap);
  provider-initiated session revocation via back-channel logout; the admin
  identity views and the pending-identity approval/bind queue (which is what
  makes `auto_provision = false` usable); RP-initiated logout; discovery-
  negotiated token-endpoint authentication.
- **Phase 1.5 (remaining, signal-driven)** — local→OIDC account conversion by
  the user (only when an install with many local users needs to keep
  `users.id`).
- **Phase 2** — multiple simultaneous providers; scoped administration;
  access reviews / SIEM export. **A Buz-side SCIM server is a permanent
  non-goal** (see [Non-goals](#non-goals)).

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
