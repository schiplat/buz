-- Web-editable SSO configuration (docs `platform/oidc-sso.md`, Phase 1.5).
-- Single-row table holding the dashboard-managed OIDC settings; the client
-- secret is stored encrypted at rest (`buzenc:v1:` via crypto::encrypt,
-- re-encrypted by rotate-jwt-key). Absent row = file `[auth.oidc]` seed
-- applies; a saved row makes the database authoritative (DB > file).

CREATE TABLE IF NOT EXISTS oidc_settings (
    id BIGINT PRIMARY KEY CHECK (id = 1),
    enabled BIGINT NOT NULL DEFAULT 0,
    issuer TEXT NOT NULL DEFAULT '',
    client_id TEXT NOT NULL DEFAULT '',
    client_secret_ciphertext TEXT NOT NULL DEFAULT '',
    redirect_uri_override TEXT,
    scopes_json TEXT NOT NULL DEFAULT '["openid","profile","email"]',
    auto_provision BIGINT NOT NULL DEFAULT 1,
    updated_by TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT datetime()
);
