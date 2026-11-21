-- Two web-editable OIDC knobs (docs `platform/oidc-sso.md`).
-- `token_auth` overrides token-endpoint client authentication ('auto' keeps
-- discovery negotiation); `groups_claim` names the id_token claim mirrored
-- into external_identities.groups_json (display/audit only — never authz).
ALTER TABLE oidc_settings ADD COLUMN IF NOT EXISTS token_auth TEXT NOT NULL DEFAULT 'auto';
ALTER TABLE oidc_settings ADD COLUMN IF NOT EXISTS groups_claim TEXT NOT NULL DEFAULT 'groups';
