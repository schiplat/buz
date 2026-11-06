---
title: Config Block (render-only)
weight: 6
description: "Nested config: block — render-only templates in place (delivery is a plugin step)"
aliases:
  - /docs/01-architecture/authoring/config-block/
---

The step-level `config:` block is **render-only**: it renders template files
that already exist under `dest`, injecting secret values and exporting
`$BUZ_CONFIG_DIR` / `$BUZ_ENV_CHANGED` to the step.

> **Removed in v3:** the kernel OSS-deliver mode (`config.source` /
> `config.key_path`) is gone — pipelines using those keys fail validation.
> Deliver config bundles with the [`buz/config-fetch`](https://github.com/schiplat/buz-plugins) plugin step
> (presigned-URL mode) or [`buz/oss-download`](https://github.com/schiplat/buz-plugins)
> (AK/SK, `engines: [docker, ssh]`), then keep a render-only `config:` block on
> the follow-up step. See [Sample A](#sample-a--fetch-plugin--render-two-step).

## YAML shape

```yaml
steps:
  - name: Render env
    engine: shell            # shell / local / ssh — not docker
    config:
      dest: /opt/manager    # workspace-relative or absolute host path
      render:
        - template: env.example
          output: .env
          mode: "0600"
          on_missing: fail
    run: |
      echo "changed=${BUZ_ENV_CHANGED:-0} keys=${BUZ_ENV_CHANGED_KEYS:-none}"
```

Validation rules:

- Nested `config:` only. Flat `config_source` / `config_path` / `config_dir` /
  `env_render` keys are rejected (unknown fields).
- `dest` ∧ `render` required together; `dest` alone or `render` alone is an error.
- `config:` is only valid on `engine: shell` / `local` / `ssh` — **not docker**
  (prepare runs on the Runner host, before the container starts; mount `dest`
  into the container with `volumes:` instead).

## Render behavior (multi-file)

1. Render-only step processes `render` entries **in order**.
2. Paths are relative to `$BUZ_CONFIG_DIR` (`dest`).
3. Any `on_missing: fail` miss → fail the step.
4. `$BUZ_ENV_CHANGED=1` if any file's managed keys changed; `_KEYS` = deduped union.
5. **Where prepare runs depends on the step engine:**
   - **`shell` / `local`:** `dest` is on the Runner machine → read templates and
     write outputs **in place**.
   - **`ssh`:** `dest` is on the remote host → fetch each `template` (+ old
     `output` for change-diff / timestamped backup) over SSH, render on the
     Runner (secrets never leave), upload outputs back.

Render is a **full rebuild**: the template is the sole source of truth, so keys
removed from the template disappear from the output. Rendered secret values are
masked in logs.

### SSH step session

Each `engine: ssh` step opens **one TCP session per host** (with connect retries
on transient reset/refused/timeout). Config prepare (fetch / render / upload)
and the step `run` block reuse that session via new exec channels — they do
**not** reconnect per remote command. Sessions are not shared across steps or
jobs. When a step resolves **multiple** hosts, fan-out follows `hosts_strategy`
(`chain` default, or `parallel`). **Cancel** aborts every in-flight host — see
[SSH Multi-Host Execution](/docs/01-architecture/execution/ssh-multi-host/).

### `engine: docker` workaround

Render on a `shell`/`ssh` step, then a later `docker` step mounts `dest`:

```yaml
volumes: ["/opt/manager:/app/config"]
```

## Runtime env

| Variable | Set on |
|----------|--------|
| `$BUZ_CONFIG_DIR` | Steps with a `config:` block (= `dest`) |
| `$BUZ_ENV_CHANGED` / `$BUZ_ENV_CHANGED_KEYS` | Same step, after render |

`on_change` / `services` keys are parsed historically but **not enforced** —
branch on `$BUZ_ENV_CHANGED` in `run` instead:

```yaml
run: |
  if [ "${BUZ_ENV_CHANGED:-0}" = "1" ]; then
    docker compose up -d --force-recreate manager-server
  fi
```

## Sample A — fetch plugin + render (two-step)

Delivery is an explicit plugin step; rendering stays a kernel `config:` block.
Canonical shape (see `example/pipelines/oss-config-demo.yml`):

```yaml
secrets:
  - CONFIG_BUNDLE_URL

steps:
  # ── 1. Fetch config bundle (presigned OSS URL → unpack) ────────────
  - name: Fetch config bundle
    uses: buz/config-fetch@1.1.0
    with:
      url: ${secrets.CONFIG_BUNDLE_URL}   # presigned, TTL <= 1h — refresh per deploy
      dest: config                        # workspace-relative

  # ── 2. Render-only (templates already under dest) ──────────────────
  - name: Render env
    engine: shell
    config:
      dest: config
      render:
        - template: env.example
          output: .env
          mode: "0600"
          on_missing: fail
    run: |
      echo "==> BUZ_CONFIG_DIR: ${BUZ_CONFIG_DIR:-<unset>}"
      ls -l "$BUZ_CONFIG_DIR/.env"
      echo "==> env changed: ${BUZ_ENV_CHANGED:-0} (${BUZ_ENV_CHANGED_KEYS:-none})"
```

For `engine: ssh` targets without runner-side workspace hand-off, use
`buz/oss-download` with `engine: ssh` (the target host pulls directly with
masked AK/SK) followed by the same render-only step.

## Sample B — Paths under `dest`

### B1 — Same root, different subdirectories (one step)

`dest` is the stack root; `template` / `output` are relative. Resolves to
`/opt/manager/.env`, `/opt/manager/nginx/nginx.conf`, etc.

```yaml
  - name: Render configs
    engine: ssh
    host: $deploy_host
    config:
      dest: /opt/manager
      render:
        - template: env.example
          output: .env
          mode: "0600"
          on_missing: fail
        - template: nginx/nginx.conf.tpl
          output: nginx/nginx.conf
          mode: "0644"
          on_missing: fail
        - template: app/config.yaml.tpl
          output: app/config.yaml
          mode: "0644"
          on_missing: warn
    run: |
      echo "changed=${BUZ_ENV_CHANGED:-0} keys=${BUZ_ENV_CHANGED_KEYS:-none}"
      # $BUZ_CONFIG_DIR == /opt/manager
```

### B2 — Unrelated roots (two steps)

| Scenario | Approach |
|----------|----------|
| Same root, different subdirs | One `dest` + relative paths (B1) |
| Unrelated roots (e.g. `/opt/manager` and `/etc/foo`) | **Two** render-only steps, each with its own `dest` |

## Sample C — Git clone + render-only

No `config.git` kernel mode — clone in `run`, then the same render-only step:

```yaml
  - name: Clone config repo
    engine: ssh
    host: $deploy_host
    run: |
      rm -rf /opt/manager
      git clone --depth 1 --branch "$INPUT_TAG" "$INPUT_CONFIG_REPO" /opt/manager
    timeout: 5m

  - name: Render env
    engine: ssh
    host: $deploy_host
    config:
      dest: /opt/manager
      render:
        - template: env.example
          output: .env
          mode: "0600"
          on_missing: fail
    run: |
      echo "changed=${BUZ_ENV_CHANGED:-0} keys=${BUZ_ENV_CHANGED_KEYS:-none}"
    timeout: 2m
```

## Related

- [Env Render](/docs/01-architecture/authoring/env-render/) — template expression reference
- [Plugin Marketplace](https://github.com/schiplat/buz-plugins) — `buz/config-fetch`, `buz/oss-download`
