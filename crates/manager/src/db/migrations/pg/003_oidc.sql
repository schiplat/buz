-- OIDC single sign-on (docs `platform/oidc-sso.md`). Users gain the
-- authentication origin (`auth_source`: `local` | `oidc`) and the password
-- gate; external identities bind IdP subjects to users; the flow/ticket
-- tables carry the browser round trip (hashed, single-use, TTL'd).

ALTER TABLE users ADD COLUMN IF NOT EXISTS auth_source TEXT NOT NULL DEFAULT 'local';
ALTER TABLE users ADD COLUMN IF NOT EXISTS password_login_disabled BIGINT NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS external_identities (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    issuer TEXT NOT NULL DEFAULT '',
    subject TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    email TEXT NOT NULL DEFAULT '',
    display_name TEXT NOT NULL DEFAULT '',
    groups_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT datetime(),
    last_login_at TEXT,
    UNIQUE (provider, subject),
    UNIQUE (provider, user_id)
);

CREATE TABLE IF NOT EXISTS sso_flows (
    id TEXT PRIMARY KEY,
    state_hash TEXT NOT NULL UNIQUE,
    nonce TEXT NOT NULL,
    code_verifier TEXT NOT NULL,
    redirect_to TEXT NOT NULL DEFAULT '/dashboard',
    expires_at TEXT NOT NULL,
    consumed_at TEXT,
    created_at TEXT NOT NULL DEFAULT datetime()
);

CREATE TABLE IF NOT EXISTS sso_tickets (
    ticket_hash TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at TEXT NOT NULL,
    consumed_at TEXT,
    created_at TEXT NOT NULL DEFAULT datetime()
);

CREATE INDEX IF NOT EXISTS idx_external_identities_user ON external_identities(user_id);
CREATE INDEX IF NOT EXISTS idx_sso_flows_expires ON sso_flows(expires_at);
CREATE INDEX IF NOT EXISTS idx_sso_tickets_expires ON sso_tickets(expires_at);
