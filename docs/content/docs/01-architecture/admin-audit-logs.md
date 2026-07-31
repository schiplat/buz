---
title: Admin Audit Logs
weight: 12
description: "Platform-admin operation audit — redacted before/after snapshots"
---

## Overview

Successful JWT mutations write best-effort rows to `audit_logs`. Platform admins can list and inspect them under **Access → Audit logs** (`/audit-logs`). Sensitive fields are redacted before insert; audit write failures never fail the primary API.

## Recorded actions (v1)

| Action | Resource |
|--------|----------|
| `auth.login` | user (never password) |
| `secret.*` / `variable.*` / `host.*` | create / update / delete |
| `user.*` / `team.*` / `team.member_*` | create / update / delete / members |
| `pipeline.*` | create / update / delete / toggle_active / trigger |
| `git_hook.*` | create / delete (no secret ciphertext) |

**Not recorded:** Runner poll/claim/heartbeat, job log upload, health, pure GETs.

## Redaction

Central helper `crates/manager/src/audit.rs` recursively masks keys such as `password`, `token`, `secret`, `value`, `credential_data`, `secret_ciphertext`, `passphrase`, `password_hash`, etc. Resource-specific snapshots:

- **Secret**: metadata + `value: "***"` (never plaintext/ciphertext)
- **Host**: credential fields masked
- **User**: no `password_hash`
- **Git hook**: public hook fields only
- **Pipeline trigger**: `after.inputs` redacts keys marked `masking: true` on the pipeline's `input_defs` (values stored as `***`). Those values are also AES-GCM encrypted in `jobs.trigger_params`.

## API

```
GET /api/v1/audit-logs?page=&page_size=&action=&actor=&resource_type=&q=&from=&to=
```

Requires **PlatformAdmin**. Response: `{ items, total, page, page_size }`. Each item includes redacted `before` / `after` JSON for the detail drawer.

See also [API Reference](/docs/01-architecture/api-reference/) and [Database Schema](/docs/02-development/database-schema/).
