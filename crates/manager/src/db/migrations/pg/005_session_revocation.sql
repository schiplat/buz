-- IdP-initiated session revocation (docs `platform/oidc-sso.md`).
-- Unix second of the last account-wide revocation the identity provider
-- delivered (back-channel logout); user JWTs minted at or before it are
-- refused on the per-request user load. API tokens are unaffected.

ALTER TABLE users ADD COLUMN IF NOT EXISTS sessions_revoked_epoch BIGINT NOT NULL DEFAULT 0;
