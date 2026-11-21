---
title: Instance Branding
weight: 16
description: "Custom logo upload — the built-in mark stays the default; a platform admin can replace it from Settings → Branding"
---

## Status

**Implemented**: a single database-backed logo (`branding`, one row), a
public read path for the sign-in pages, and a platform-admin upload/reset
page. The built-in mark is always the default: an absent row means the SPA
renders its bundled logo, and **nothing on this page changes the product
name, the sidebar text, or the theme**.

## What administrators can set

Only the logo image. The page lives at **Settings → Branding**
(`/settings/branding`) and is visible to platform admins only.

| Action | Endpoint | Who |
|---|---|---|
| Read the descriptor | `GET /api/v1/branding` | public (sign-in needs it) |
| Read the image | `GET /api/v1/branding/logo` | public |
| Upload / replace | `PUT /api/v1/admin/branding` | platform admin |
| Restore the built-in mark | `DELETE /api/v1/admin/branding` | platform admin |

`GET /api/v1/branding` carries the upload guidance (`recommendations`) so the
copy shown in the UI and the limits the server enforces cannot drift:

| Item | Recommendation |
|---|---|
| Format | **SVG** or **PNG**; WebP and JPEG are also accepted |
| Dimensions | **512 × 512 px, square** |
| Minimum | 128 × 128 px (the mark renders at 32 px in the sidebar and 40 px on sign-in pages) |
| File size | at most **512 KB** (smaller is better) |
| Background | transparent (JPEG has no alpha channel) |
| Safe margin | at least 10% on every side |
| Shape | square icon mark — a horizontal wordmark is **not** supported |

## Storage

One row in `branding` (`id = 1`), the image as base64 text: one statement
body serves both SQLite and Postgres, and the size cap keeps the inline value
small. The row carries `logo_mime`, `logo_bytes`, `updated_by` and
`updated_at`; the upload's content type is restricted to an allowlist
(`image/png`, `image/jpeg`, `image/webp`, `image/svg+xml`) and the bytes must
match the declared format (magic bytes; structural checks for SVG).

## Caching and rendering

* `GET /api/v1/branding/logo` answers `Cache-Control: no-cache` with a
  content-derived `ETag`, and honours `If-None-Match` with `304`. The SPA
  appends the descriptor's `etag` as `?v=…`, so a replaced logo appears
  immediately without trading freshness for cacheability.
* The same image becomes the browser tab icon while it is configured;
  restoring the built-in mark restores `/favicon.svg`.
* All response bytes are served with `X-Content-Type-Options: nosniff` and a
  restrictive CSP (`default-src 'none'; style-src 'unsafe-inline'; img-src data:`),
  so a crafted SVG cannot execute even when opened directly.

## Upload validation

1. Base64 length is checked **before** decoding (a 512 KB image is ~683 KB
   of base64; larger bodies are rejected without allocating).
2. Decoded size must be non-empty and at most 512 KB.
3. Declared MIME must be on the allowlist and match the file's magic bytes.
4. SVG downloads are rejected when they contain `<!DOCTYPE`, `<!ENTITY`,
   `<script`, `<foreignObject`, `javascript:`, inline `on*=` handlers, or an
   `href` that leaves the document (only in-document `#id` and `data:image/…`
   references are allowed). A file that trips the list must be re-exported —
   there is no sanitising pass.

The audit log records `branding.update` / `branding.reset` with the MIME type
and size only — never the image bytes.

## Non-goals

* No product-name or subtitle replacement; the sidebar keeps “Buz ·
  Orchestration” and the footer keeps its standard format.
* No theme/accent-colour editing.
* No manager.toml seed — the database is the only source, matching the
  current SSO configuration model.
* No per-team or per-user branding.
