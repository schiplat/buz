---
title: Config Block DSL v2
weight: 6
description: "Shipped — nested config: delivery + multi-file render (OSS / render-only)"
---

> **Status: SHIPPED — Phase 2b complete (D1–D5).**  
> Nested `config:` is the only authoring shape. DB/API/Runner use
> `config_key_path` / `config_dest` / `config_render` (legacy columns dropped).
> Render-only prepare works on shell/local (in place) and ssh (fetch→render→upload).
> UI: Create/Edit Config modes + multi-row render.  
> **Not in 2b:** `config.git` (2c), auto `on_change`/`services` (2d).  
> See also [Env Render](/docs/01-architecture/env-render/) (Phase 1 engine).

## Goals

1. Remove the `config_path` vs `config_dir` naming clash (OSS key vs host path).
2. Support **multiple** render outputs in one step.
3. Prefer **delivery and render as separate steps** (same shape for OSS and git).
4. Hard cut: new YAML + new DB columns only (no dual-read, no flat import).

## Authoring model

```text
[optional Validate]
    → Deliver (OSS unpack  |  git clone run  |  future config.git)
    → Render-only (config.dest + config.render)
    → Compose / app steps
    → Health
```

| Mode | `config:` fields | Prepare | Phase |
|------|------------------|---------|-------|
| **OSS deliver** | `source` + `key_path` + `dest` | signed GET → unpack to `dest` | **2b** |
| **render-only** | `dest` + `render` | templates already at `dest` → render | **2b** |
| **OSS+render one-step** | all of the above | unpack then render | 2b sugar — **not** used in docs/examples |
| **`config.git`** | `git` + `dest` (+ optional later render) | first-class clone | **2c** |

### Validation rules (2b)

- Nested `config:` only. Flat `config_source` / `config_path` / `config_dir` /
  `env_render` → **validation error** (YAML import included).
- A `config:` block must be exactly one of:
  - **deliver:** `source` ∧ `key_path` ∧ `dest` (no `render` in preferred form), or
  - **render-only:** `dest` ∧ `render` and **no** `source` / `key_path`.
- **`config.render` / render-only are invalid on `uses: docker`** (shell /
  local / ssh only). See [Engines](#engines-render-only-does-not-include-docker).
- One-step sugar (`source` + `key_path` + `dest` + `render`) may be accepted by
  the engine but must not appear in examples or Quick YAML Reference.

### Field reference

| YAML | DB column | Meaning |
|------|-----------|---------|
| `config.source` | `config_source` | OSS auth secret name |
| `config.key_path` | `config_key_path` | OSS object-key **prefix** (Manager appends `/configs.tar.gz`) |
| `config.dest` | `config_dest` | Unpack / working dir → `$BUZ_CONFIG_DIR` |
| `config.render` | `config_render` (JSON array) | List (or single object → normalized to 1-element array) |

| `render[]` field | Required in examples | Runtime default if omitted |
|------------------|----------------------|----------------------------|
| `template` | yes | — (required) |
| `output` | yes | `<template>.rendered` |
| `mode` | yes (show always) | `"0600"` |
| `on_missing` | yes (show always) | `fail` (`fail` \| `warn`) |

**Not in 2b examples or enforcement:** `on_change` / `services` (see below).

Runtime env names unchanged: `$BUZ_CONFIG_DIR`, `$BUZ_CONFIG_URL` (OSS deliver),
`$BUZ_ENV_CHANGED`, `$BUZ_ENV_CHANGED_KEYS`.

### DB migration (one shot)

1. `ADD` `config_key_path`, `config_dest`, `config_render`.
2. Backfill from `config_path` / `config_dir` / `env_render` (object → `[obj]`).
3. `DROP` `config_path`, `config_dir`, `env_render` in the **same** upgrade.
4. Claim / Runner JSON: new names only. Upgrade Manager + Runner together.

### Render behavior (multi-file)

1. Delivery step (if any) places files under `dest` once.
2. Render-only step processes `render` entries **in order**.
3. Paths are relative to `$BUZ_CONFIG_DIR` (`dest`).
4. Any `on_missing: fail` miss → fail the step.
5. `$BUZ_ENV_CHANGED=1` if any file’s managed keys changed; `_KEYS` = deduped union.
6. **Where prepare runs depends on the step engine** (not “SSH-only”):
   - **`shell` / `local`:** `dest` is on the Runner machine → read templates and
     write outputs **in place** (same as today’s local OSS prepare).
   - **`ssh`:** `dest` is on the remote host → fetch each `template` (+ old
     `output` for merge) over SSH, render on the Runner (secrets never leave),
     upload outputs back. Same trust model as today’s SSH + OSS path.
   - **`docker`:** **not supported for `render` / render-only in 2b** (see below).

### Engines: render-only does **not** include `docker`

| Engine | OSS deliver (`source` + `key_path` + `dest`) | `render` / render-only |
|--------|-----------------------------------------------|-------------------------|
| `shell` / `local` | yes | **yes** (in place) |
| `ssh` | yes | **yes** (fetch → render → upload) |
| `docker` | may keep today’s deliver-only allowance | **no** in 2b |

**Why:** prepare runs **before** the container `run`. Templates must already
exist on a host filesystem the Runner can read. A `uses: docker` workdir lives
inside the container unless `dest` is defined as a host path bind-mounted in —
that contract is not designed yet.

**Workaround:** render on `shell`/`ssh`, then a later `docker` step mounts
`dest` (e.g. `volumes: ["/opt/manager:/app/config"]`).

Validation must reject `config.render` (and render-only `config:`) on
`uses: docker`. Do not document docker+render samples until a later phase
defines host-`dest` + volume semantics.

---

## Sample A — OSS deploy (preferred two-step)

Canonical shape for `super-manager-deploy-host-b` after 2b lands:

```yaml
name: super-manager deploy host-b
description: Deploy manager-server + postgres + redis (OSS config + render).
team: default

secrets:
  - POSTGRES_PASSWORD

on:
  manual:
    inputs:
      tag:
        description: Image / config bundle tag
        required: true
        default: "1.4.0"
      deploy_host:
        description: Buz Host name
        required: true
        default: "host-b"

runner:
  labels: [linux-x64]
  timeout: 20m

env:
  CONFIG_DIR: /opt/manager

steps:
  - name: Validate tag
    uses: shell
    run: |
      case "$INPUT_TAG" in
        ''|*[!A-Za-z0-9._-]*)
          echo "ERROR: invalid tag '$INPUT_TAG'" >&2
          exit 1
          ;;
      esac
      echo "tag OK: $INPUT_TAG"
    timeout: 30s

  # ── Deliver: OSS unpack only ──
  - name: Deliver config
    uses: ssh
    host: $deploy_host
    config:
      source: OSS_CONFIG
      key_path: config_tpl/super-manager/$tag
      dest: /opt/manager
    run: |
      ls -l "$BUZ_CONFIG_DIR/docker-compose.yml" "$BUZ_CONFIG_DIR/env.example"
    timeout: 5m

  # ── Render: templates already on disk ──
  - name: Render env
    uses: ssh
    host: $deploy_host
    config:
      dest: /opt/manager
      render:
        - template: env.example
          output: .env
          mode: "0600"
          on_missing: fail
    run: |
      ls -l "$BUZ_CONFIG_DIR/.env"
      echo "==> env changed: ${BUZ_ENV_CHANGED:-0} (${BUZ_ENV_CHANGED_KEYS:-none})"
    timeout: 2m

  # Dedicated Compose step (preferred over render on_change).
  # --force-recreate so .env rotations apply; $BUZ_ENV_CHANGED is step-local.
  - name: Compose up
    uses: ssh
    host: $deploy_host
    registry_secret: ACR_PERSONAL
    run: |
      cd "$CONFIG_DIR"
      docker compose pull
      docker compose up -d --force-recreate --remove-orphans
      docker compose ps
    timeout: 15m

  - name: Health check
    uses: ssh
    host: $deploy_host
    run: |
      sleep 12
      curl -fsS http://127.0.0.1:8787/health && echo || echo "not ready yet"
      docker compose -f "$CONFIG_DIR/docker-compose.yml" exec -T postgres pg_isready -U postgres -d super_manager
      docker compose -f "$CONFIG_DIR/docker-compose.yml" exec -T redis redis-cli ping
    continue-on-error: true
    timeout: 2m
```

---

## Sample B — Multi-file render

```yaml
  - name: Render configs
    uses: ssh
    host: $deploy_host
    config:
      dest: /opt/manager
      render:
        - template: env.example
          output: .env
          mode: "0600"
          on_missing: fail
        - template: nginx.conf.tpl
          output: conf/nginx.conf
          mode: "0644"
          on_missing: fail
        - template: redis.conf.tpl
          output: conf/redis.conf
          mode: "0644"
          on_missing: warn
    run: |
      echo "changed=${BUZ_ENV_CHANGED:-0} keys=${BUZ_ENV_CHANGED_KEYS:-none}"
```

---

## Sample C — Git clone + render-only (until 2c)

No `config.git` yet — clone in `run`, then the same render-only step as OSS:

```yaml
  - name: Clone config repo
    uses: ssh
    host: $deploy_host
    run: |
      rm -rf /opt/manager
      git clone --depth 1 --branch "$INPUT_TAG" "$INPUT_CONFIG_REPO" /opt/manager
    timeout: 5m

  - name: Render env
    uses: ssh
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

---

## Sample D — Minimal OSS smoke (deliver + list)

```yaml
  - name: Deliver demo bundle
    uses: shell
    config:
      source: OSS_DEMO
      key_path: config_tpl/demo/$tag
      dest: /tmp/buz-config-demo
    run: |
      ls -la "$BUZ_CONFIG_DIR"
```

---

## Out of scope for 2b

| Item | Why |
|------|-----|
| **`render` / render-only on `uses: docker`** | Prepare needs host-visible `dest`; container-only paths undefined — use shell/ssh render + volume mount |
| **`on_change` / `services` auto recreate** | Not required for 2b — see next section |
| **`config.git`** | Phase **2c** (credentials, clone locus, UI) |
| Rename `$BUZ_CONFIG_DIR` | Keep for script compatibility |
| Flat YAML import rewrite | Reject old shape instead |

## `on_change` / `services` — ship with 2b?

**No — do not bundle into 2b.**

| | |
|--|--|
| What they are | After render, auto `docker compose up -d --force-recreate <services>` when managed keys change |
| Today | Parsed in YAML historically; **not enforced**. Authors branch on `$BUZ_ENV_CHANGED` / `_KEYS` in `run` |
| Cost if added now | Extra compose execution path on SSH/shell, multi-file “which services?”, conflict with a following Compose step, more UI + tests |
| 2b already delivers | Nested DSL, DB cut-over, multi-file render, OSS deliver, render-only, clear step split |

**Recommendation:** keep using:

```yaml
run: |
  if [ "${BUZ_ENV_CHANGED:-0}" = "1" ]; then
    docker compose up -d --force-recreate manager-server
  fi
```

Revisit auto `on_change`/`services` as **2d** (or finish Phase 2) after 2b is stable.
**Do not** put `on_change` / `services` in samples or Quick YAML Reference until enforced.

## Implementation checklist

| # | Area | Notes | Status |
|---|------|-------|--------|
| 1–4 | Create / Edit / import / Definition UI | Nested Config; deliver vs render-only modes; multi-row render; **show** `template`/`output`/`mode`/`on_missing` | ✅ |
| 5 | `pipeline_yaml` | `ConfigBlock`; reject flat fields; validate deliver XOR render-only (+ optional sugar); **reject `render` on `docker`** | ✅ |
| 6–7 | `pipelines.rs` + claim | New columns; expand `key_path`; `config_auth` only for OSS deliver | ✅ |
| 8 | Runner prepare | OSS unpack; multi-file render loop; **shell/local in-place** + **SSH fetch/render/upload** | ✅ |
| 9 | TS types | `config_source` / `config_key_path` / `config_dest` / `config_render` | ✅ |
| 10–11 | `example/pipelines/*` + Quick Ref | Nested `config:` in examples + Pipelines Quick YAML Reference | ✅ |
| — | Docs | This page + env-render + pipeline-format `config` section | ✅ |

## Schedule (Phase 2b)

Estimate for **one focused developer**, tests interleaved (not deferred to the
end). **Plan on ~5 working days**; tight path ~4d, buffer to ~5.5d.

| Day | Deliverables | Tests (same day) |
|-----|--------------|------------------|
| **D1** ✅ | `pipeline_yaml` + protocol: `ConfigBlock`, `render[]` / single-object sugar, reject flat fields, deliver XOR render-only | Unit: valid/invalid YAML; multi-file fields; mixed flat+nested rejected |
| **D2** ✅ | DB one-shot migration (`config_key_path` / `config_dest` / `config_render`, backfill, **DROP** old cols); `pipelines` CRUD/GET; claim expands `key_path`, attaches `config_auth` **only** for OSS deliver; FE types + form fields renamed (no dual-read) | Claim JSON new names only; shared/manager/runner tests green |
| **D3** ✅ | Runner prepare: **render-only without OSS**; SSH fetch template → render on Runner → upload; multi-file `BUZ_ENV_CHANGED` union | `config_source_*` / poll: render-only; missing dest/template; `on_missing` warn |
| **D4** ✅ | Nested Config UI + multi-row render editor + Definition polish (Quick YAML Ref already nested in D2) | `configForm` vitest; form modes for deliver/render; `npm run build` |
| **D5** ✅ | Integration + doc polish + CI gate | `cargo test --all`; `example_pipelines_validate_test`; `npm test` + `npm run build` |

Two developers (backend ∥ frontend) can compress calendar time to ~**3–3.5 days**,
still reserving half a day for joint integration.

### Testing cadence

1. **Per layer before merge:** shared → manager → runner → frontend tests green
   for that slice.
2. **After D2:** boot Manager against a DB that had legacy columns; confirm DROP
   + backfill; GET pipeline shows new field names.
3. **After D3:** exercise at least:
   - OSS **deliver** → **render-only** (shell and/or SSH)
   - **render-only** alone (pre-seeded `dest`)
   - Failures isolated: OSS 404 on deliver step; missing `${secrets.X}` on render step
4. **After D4:** YAML import **new shape only**; UI can edit deliver vs multi-row render.
5. **Exit gate (D5 — passed):** `cargo test --all`,
   `example_pipelines_validate_test`, `cd frontend && npm test && npm run build`.

### Acceptance (D5)

| Check | Result |
|-------|--------|
| All example pipelines validate (`example/pipelines/*.yml`) | ✅ |
| Shared / manager / runner tests green | ✅ |
| Frontend vitest + `npm run build` | ✅ |
| YAML import rejects flat `config_path` / `env_render` | ✅ (D1) |
| Form can author deliver vs multi-row render | ✅ (D4) |
| Claim attaches `config_auth` only for OSS deliver | ✅ (D2) |
| Render-only prepare without OSS download | ✅ (D3) |

### Runner scope reminder (D3)

| Engine | `dest` location | Prepare |
|--------|-----------------|---------|
| `shell` / `local` | Runner filesystem | In-place read/write under `dest` |
| `ssh` | Remote host | Fetch templates (+ old output) → render on Runner → upload outputs |
| `docker` | — | **No `render` / render-only** in 2b |

Do **not** treat render-only as an SSH-only feature; also do **not** enable it
for `docker` without a host-`dest` + volume design.

### Schedule risks (+0.5–1d)

- SSH fetch/upload edge cases (permissions, missing template, multi-host)
- Frontend mode switch (deliver vs render-only) rework
- Real DB migration + claim/snapshot integration surprises

### Not in this schedule

| Phase | Item | Rough add-on |
|-------|------|--------------|
| **2c** | `config.git` delivery | ~2–3d |
| **2d** | Enforce `on_change` / `services` | ~1–2d |

## Related

- [Env Render](/docs/01-architecture/env-render/) — Phase 1 engine, ValueMap, security
- [Secrets Access Control](/docs/01-architecture/secrets-access-control/) — OSS secret kind, verbose/debug
- [Pipeline Format](/docs/01-architecture/pipeline-format/)
