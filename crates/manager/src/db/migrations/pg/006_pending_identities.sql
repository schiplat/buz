-- Identities refused at sign-in because `auto_provision` is off
-- (docs `platform/oidc-sso.md`). One row per (provider, subject), refreshed
-- on every refused attempt; an administrator approves it from the Users page
-- (create a user or bind to an existing one) or dismisses it.

CREATE TABLE IF NOT EXISTS pending_identities (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    issuer TEXT NOT NULL DEFAULT '',
    subject TEXT NOT NULL,
    email TEXT NOT NULL DEFAULT '',
    display_name TEXT NOT NULL DEFAULT '',
    groups_json TEXT NOT NULL DEFAULT '[]',
    first_seen_at TEXT NOT NULL DEFAULT datetime(),
    last_seen_at TEXT NOT NULL DEFAULT datetime(),
    attempts BIGINT NOT NULL DEFAULT 1,
    UNIQUE (provider, subject)
);
