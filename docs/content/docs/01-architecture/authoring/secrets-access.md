---
title: Secrets in Pipelines (GHA-style)
weight: 20
description: Allowlist secrets, map via env or purpose slots, expand ${secrets.X} / ${vars.X}
aliases:
  - /docs/01-architecture/secrets-access/
  - /docs/01-architecture/platform/secrets-access/
---

Buz uses the same **allowlist → explicit map → consume** flow as GitHub Actions, with expression syntax aligned to config render: `${secrets.X}` / `${vars.X}` (not `${{ }}`).

## Rules

1. **Allowlist / reference ≠ inject.** Listing a name (or referencing `${secrets.NAME}`) does **not** put `$NAME` into the process environment by itself.
2. **Map before use.** Put values into the environment with `env:`. Registry auth and config delivery both use plugin templates (`buz/docker-login`, `buz/config-fetch`) instead of purpose slots — the kernel purpose slots were removed in the 2.x retirement.
3. **Scripts use shell vars.** After `env: GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}`, `run` uses `$GIT_SSH_KEY`. Do not write `${secrets.…}` inside `run` — the engine does not expand secrets inside scripts.
4. **Render templates get their values via `buz/env-render`'s `values` map.** One `NAME=${secrets.NAME}` line per referenced secret in `with:` — the kernel `config.render` block still works for existing pipelines, but the plugin is the recommended path.
5. **Team-shared secrets** are auto-**allowlisted** for expression resolution; they are still not auto-injected as env vars.
6. **Top-level `secrets:` is optional** for references the YAML scanner can see — step `env:` maps and `uses:` steps' `with:` inputs are collected automatically (GHA-style). It remains **required** when a `config.render` template references secrets: those references live inside external template files no YAML scan can see, so the declaration is the injection allowlist for external templates. Full cheat sheet: [Secrets & Variables Expressions](../authoring/secrets-vars-expressions.md).

## Pipeline shape

```yaml
secrets:
  - GIT_SSH_KEY
  - OSS_CONFIG
  - POSTGRES_PASSWORD

env:
  # Optional pipeline-level map (values expanded at claim time)
  # GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}

steps:
  - name: Clone Private Repo
    secrets: [GIT_SSH_KEY]   # optional step allowlist (narrowing)
    env:
      GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}
    run: |
      echo "$GIT_SSH_KEY" > /tmp/key && chmod 600 /tmp/key
      GIT_SSH_COMMAND="ssh -i /tmp/key -o StrictHostKeyChecking=no" \
        git clone --depth 1 git@github.com:org/private.git /tmp/private
      rm -f /tmp/key

  - name: Render env
    uses: buz/env-render@1.0.0
    engine: ssh
    host: $deploy_host
    with:
      dest_dir: /opt/app
      template: env.example   # file uses ${secrets.POSTGRES_PASSWORD}
      output: .env
      mode: "0600"
      on_missing: fail
      values: |
        POSTGRES_PASSWORD=${secrets.POSTGRES_PASSWORD}
```

## Purpose slots

| Slot | YAML | Meaning |
|------|------|---------|


The kernel purpose slots (`registry_secret`, `config.source`) were removed in the 2.x retirement. Registry login uses the `buz/docker-login` plugin template with two plain `generic` secrets; config delivery uses `buz/config-fetch` (presigned URL) or `buz/oss-download` (AK/SK) — both reference secrets the same way plugins always have.

## Allowlist scope

| Source | Effect |
|--------|--------|
| Use-site `${secrets.NAME}` / purpose slot | **Implicit allowlist** for that name (same team) |
| Step `secrets: [NAME]` | Explicit step allowlist (optional) |
| Pipeline top-level `secrets:` | Optional overview + `pipeline_secrets` bind |
| `team_shared: true` | Auto-allowlisted for every same-team job |

Cross-team names always fail. Missing same-team secret → claim / expand error.

## Related

- [Secrets & Variables — Expression Cheat Sheet](../authoring/secrets-vars-expressions.md) — **all syntaxes in one place**
- [Pipeline YAML format](../authoring/pipeline-format.md)
- [Secrets Access Control](./secrets-access-control.md) — kind, team_shared, bindings
- [Config Block & Env Files](../authoring/config-block.md) — Deliver / Render / `${secrets.X}` in templates
- [YAML-First Authoring](../authoring/yaml-first-authoring.md) — YAML sole orchestration source (file authority)
