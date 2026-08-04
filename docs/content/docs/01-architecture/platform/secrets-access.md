---
title: Secrets Access (GHA-style)
weight: 3
description: Allowlist secrets, map via env or purpose slots, expand ${secrets.X} / ${vars.X}
aliases:
  - /docs/01-architecture/secrets-access/
---

# Secrets Access (GHA-style)

Buz uses the same **allowlist → explicit map → consume** flow as GitHub Actions, with expression syntax aligned to config render: `${secrets.X}` / `${vars.X}` (not `${{ }}`).

## Rules

1. **Allowlist / reference ≠ inject.** Listing a name (or referencing `${secrets.NAME}`) does **not** put `$NAME` into the process environment by itself.
2. **Map before use.** Put values into the environment with `env:`, or bind a **purpose slot** (`registry_secret`, `config.source`) to an expression.
3. **Scripts use shell vars.** After `env: GIT_SSH_KEY: ${secrets.GIT_SSH_KEY}`, `run` uses `$GIT_SSH_KEY`. Do not write `${secrets.…}` inside `run` — the engine does not expand secrets inside scripts.
4. **Templates still use expressions.** `config.render` templates may contain `${secrets.X}` / `${vars.X}`; the runner expands them when generating files.
5. **Team-shared secrets** are auto-**allowlisted** for expression resolution; they are still not auto-injected as env vars.
6. **Top-level `secrets:` is optional.** Prefer GHA-style production YAML: omit the pipeline list and reference secrets at the step (`env:` / purpose slots). A top-level list remains useful as an optional overview. Full cheat sheet: [Secrets & Variables Expressions](../authoring/secrets-vars-expressions.md).

## Pipeline shape

```yaml
secrets:
  - GIT_SSH_KEY
  - ACR_PERSONAL
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

  - name: Docker Push
    uses: shell
    secrets: [ACR_PERSONAL]
    registry_secret: ${secrets.ACR_PERSONAL}
    run: |
      docker push example.com/ns/app:$INPUT_TAG

  - name: Deliver config
    uses: ssh
    host: $deploy_host
    secrets: [OSS_CONFIG]
    config:
      source: ${secrets.OSS_CONFIG}
      key_path: config_tpl/app/$tag
      dest: /opt/app
    run: ls "$BUZ_CONFIG_DIR"

  - name: Render env
    uses: ssh
    host: $deploy_host
    secrets: [POSTGRES_PASSWORD]
    config:
      dest: /opt/app
      render:
        - template: env.example   # file uses ${secrets.POSTGRES_PASSWORD}
          output: .env
          mode: "0600"
          on_missing: fail
    run: ls "$BUZ_CONFIG_DIR/.env"
```

## Purpose slots

| Slot | YAML | Meaning |
|------|------|---------|
| Registry login | `registry_secret: ${secrets.NAME}` | Docker registry credentials secret (kind `registry`) |
| Config OSS | `config.source: ${secrets.NAME}` | Object-storage secret for Deliver / SSH fetch |

Bare names (`registry_secret: ACR_PERSONAL`) remain accepted for migration; prefer the expression form.

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
- [Config Block](../authoring/config-block.md) — Deliver / Render / `${secrets.X}` in templates
- [YAML-First Authoring](../authoring/yaml-first-authoring.md) — YAML sole orchestration source (file authority)
