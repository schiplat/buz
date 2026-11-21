-- Instance branding (docs `platform/branding.md`).
-- Single-row table holding the administrator-uploaded logo. Absent row = the
-- built-in mark is used, so the default look is untouched. The image is stored
-- as base64 text: portable across both backends and small enough to inline
-- (the API caps it at 512 KB).

CREATE TABLE IF NOT EXISTS branding (
    id BIGINT PRIMARY KEY CHECK (id = 1),
    logo_mime TEXT NOT NULL DEFAULT '',
    logo_base64 TEXT NOT NULL DEFAULT '',
    logo_bytes BIGINT NOT NULL DEFAULT 0,
    updated_by TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT datetime()
);
