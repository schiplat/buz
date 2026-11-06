---
title: v3 Migration Guide
weight: 3
description: "Migrating off the retired kernel registry_secret and config.source paths"
---

Buz v3 removes two kernel features that were deprecated since 2.x:

1. **`registry_secret`** (kernel docker login/logout sandwich) →
   [`buz/docker-login`](https://github.com/schiplat/buz-plugins) plugin template
2. **`config.source` / `config.key_path`** (kernel OSS deliver + Manager
   signed-URL attachment + clock-skew guard) →
   [`buz/config-fetch`](https://github.com/schiplat/buz-plugins) (presigned URL)
   or [`buz/oss-download`](https://github.com/schiplat/buz-plugins) (AK/SK)

Pipelines that still use these keys **fail YAML validation** after the
upgrade (unknown fields are rejected). Everything else about your
pipelines is unchanged; the `config:` block itself remains, now
render-only.

## Step 1 — scan before upgrading

Run the read-only scan against the **old** binary (any 2.x release) or
against the database files:

```bash
buz-manager scan-deprecated
```

It lists every affected pipeline step, YAML file, and secret kind, plus
the plugin-path migration for each. Nothing is modified.

## Step 2 — migrate pipelines

**Registry login** — replace the `registry_secret:` field with a
`buz/docker-login` step before the pull/push steps:

```yaml
# Before (removed in v3)          # After
steps:                            steps:
  - name: deploy                    - name: Registry login
    engine: docker                    uses: buz/docker-login@1.1.0
    image: cr.example.com/app:1       with:
    registry_secret: ACR_PERSONAL       registry: ${secrets.REGISTRY_HOST}
    run: ...                            username: ${secrets.REGISTRY_USERNAME}
                                        password: ${secrets.REGISTRY_PASSWORD}
                                      - name: deploy
                                        engine: docker
                                        image: cr.example.com/app:1
                                        run: ...
```

Split the old JSON secret (`{username, password, registry}`) into two
plain `generic` secrets (`REGISTRY_USERNAME`, `REGISTRY_PASSWORD`,
optional `REGISTRY_HOST`).

**Config delivery** — replace `config.source`/`config.key_path` with a
fetch step + a render-only block:

```yaml
# Before (removed in v3)          # After
steps:                            steps:
  - name: deploy                    - name: Fetch config bundle
    engine: shell                     uses: buz/config-fetch@1.1.0
    config:                           with:
      source: OSS_CONFIG                url: ${secrets.CONFIG_BUNDLE_URL}
      key_path: config_tpl/app/v1       dest: config
      dest: /opt/app                  - name: deploy
      render: [...]                     engine: shell
    run: ...                            config:
                                          dest: config        # render-only
                                          render: [...]       # unchanged
                                        run: ...
```

The `render` entries, `$BUZ_CONFIG_DIR`, and `$BUZ_ENV_CHANGED`
semantics are unchanged. For `engine: ssh` targets that must pull
directly, use `buz/oss-download` (masked AK/SK, `engines: [docker, ssh]`).

## Step 3 — migrate secrets

| Legacy kind (pre-v3 rows) | Action |
|---------------------------|--------|
| `registry` (JSON creds) | Split into `REGISTRY_USERNAME` / `REGISTRY_PASSWORD` generic secrets (or delete) |
| `aliyun_oss_auth` (JSON AK/SK) | Delete, or keep the AK/SK as a `generic` secret for `buz/oss-download` |

Stored rows keep their legacy kind value and behave as `generic`; the
dashboard still badges them for visibility. New secrets are created as
`generic` only.

## What was removed, exactly

| Retired | Replacement |
|---------|-------------|
| `registry_secret` step field + kernel login/logout | `buz/docker-login` step |
| `BUZ_REGISTRY_*` env on ssh steps | template `with:` inputs |
| `config.source` / `config.key_path` + signed-URL claim | `buz/config-fetch` / `buz/oss-download` step |
| Secret kinds `registry` / `aliyun_oss_auth` | plain `generic` secrets |
| Manager `[clock]` guard + `/health` clock segment | NTP on the Manager host (guard existed only to protect signing) |
| Runner `[clock]` skew monitor | NTP on Runner hosts |
| `tasks.config_source` / `config_key_path` DB columns | retained (inert, legacy data) |

Trust-model note: with the signed-URL path gone, object-store
credentials (presigned URL or AK/SK) reach the Runner as masked plugin
inputs — the same model as mainstream CI. Use short-TTL presigned URLs
and least-privilege keys (see SECURITY.md in the repository root, “Known limitations”).
