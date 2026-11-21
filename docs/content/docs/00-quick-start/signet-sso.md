---
title: SSO in Ten Minutes with Signet
weight: 15
description: Connect Buz to Signet, the OIDC provider this integration is verified against
---

Buz speaks **generic OIDC** — any standards-compliant provider works. This page
walks the path we test end-to-end: [Signet](https://github.com/schiplat/signet),
a self-hosted OIDC identity provider. The same steps apply to any other
provider; only the issuer URL and the client-registration UI change.

## 1. Run Signet

Signet needs a PostgreSQL database. Its release assets and deploy stack live in
the [Signet repository](https://github.com/schiplat/signet):

- **Binary:** download the release asset for your platform and run it next to a
  PostgreSQL 16 instance (Signet's README has the exact connection string).
- **Docker:** the `deploy/` compose stack pulls a released image and connects to
  an existing PostgreSQL (`SIGNET_IMAGE`, `SIGNET_DATABASE_URL`).

For a local trial, open **http://localhost:8443** — the first visit redirects to
`/setup`, where you create the first administrator. Leave the issuer at the
default for local use; for a real deployment set it under **Dashboard →
Settings → Server identity** (HTTPS origin, no trailing slash).

## 2. Register a Buz client in Signet

In the Signet dashboard open **Clients → New client** and register Buz with:

| Field | Value |
| --- | --- |
| Redirect URI | `{BUZ_BASE_URL}/api/v1/auth/oidc/callback` |
| Post-logout redirect URI | `{BUZ_BASE_URL}/auth/sso/logged-out` |
| Back-channel logout URI | `{BUZ_BASE_URL}/api/v1/auth/oidc/backchannel-logout` |
| Scopes | `openid profile email` |
| Client authentication | `client_secret_post` (Buz negotiates if the provider only offers `client_secret_basic`) |

`{BUZ_BASE_URL}` is the public origin users reach Buz at (for example
`https://buz.example.com`). The Buz SSO page prints the same three URIs under
**Registration**, so you can copy them instead of typing them.

Keep the client ID and secret — they are shown once.

## 3. Configure Buz

In Buz open **Settings → SSO → Provider connection** and fill in:

- **Issuer:** the Signet origin, e.g. `http://localhost:8443` (no trailing slash)
- **Client ID / Client secret:** from step 2
- **Scopes:** `openid profile email`

Two optional fields usually stay at their defaults:

- **Token endpoint auth:** `Auto (from discovery)` — override only if the
  provider's discovery document is wrong or silent.
- **Groups claim:** `groups` — change it if Signet emits the list under a
  different claim name. The claim is mirrored for display/audit only; it
  never grants permissions.

The sidebar's **Access policy** card sets the session lifetime (hours) and the
client-side idle timeout (minutes, 0 = off). Both are global; the lifetime
applies to local and SSO sign-ins alike, and the idle timeout signs the
browser out without revoking the token.

Save, then click **Probe**. A green result means the discovery document,
JWKS, token endpoint, and end-session endpoint were all reachable from the
Manager.

## 4. Verify the round trip

1. Sign out of Buz.
2. On the login page choose **Sign in with SSO**.
3. Sign in at Signet; the browser returns to Buz and the session is created.
4. Open **Users & teams** to see the account (auto-provisioned, or queued for
   approval when **Create on first sign-in** is off).

Sign out again and confirm Signet asks for credentials: Buz uses
RP-initiated logout when the provider publishes an `end_session_endpoint`
(Signet does). Disabling the user in Signet also revokes live Buz sessions
through back-channel logout.

## 5. Choose the sign-in policy

**Settings → SSO → Sign-in policy** has two modes:

- **Create on first sign-in** — a new SSO identity becomes an ungrouped account
  immediately; grant it a team afterwards under **Users & teams**.
- **Queue for approval** — refused sign-ins are recorded in the approval queue
  on **Users & teams**; a platform admin binds the identity to an existing
  account or creates a new one, then grants a team.

## Troubleshooting

- **Redirect URI mismatch** — the URI registered in Signet must match
  `{BUZ_BASE_URL}` exactly, including scheme, host, port, and path.
- **Probe fails** — the Manager (not your browser) must be able to reach the
  issuer. `localhost` in a container refers to the container; use the host name
  or the host's network address.
- **Login loops back** — the issuer stored in the client must equal the issuer
  published in discovery. Signet's saved **Server identity** value wins over
  `SIGNET_ISSUER`; align Buz with that value.
- **Production** — terminate TLS in front of both services. HTTPS issuers make
  Signet mark its cookies `Secure`; Buz's callback must also be HTTPS so the
  session cookie survives the round trip.

Other providers are supported through the same generic OIDC path — see
[OIDC Single Sign-On](/docs/01-architecture/platform/oidc-sso/) for the design
and the security invariants. Signet is the provider this integration is
end-to-end tested against; per-provider verification is tracked as adoption
grows.
