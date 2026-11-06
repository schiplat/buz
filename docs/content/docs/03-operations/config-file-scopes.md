---
title: Config File Scopes
weight: 6
description: "Configuration-file parameters only (manager.toml / runner.toml) — where each key is declared, what it takes effect on, who can override it. Pipeline YAML parameters live in Authoring."
aliases:
  - /docs/03-operations/config-file-scopes/
---

# Config File Scopes

This page covers the two **configuration files** only — one per process:

| File | Process | Takes effect on |
|---|---|---|
| `conf/manager.toml` | Manager | **one Manager process — all teams and pipelines it serves** |
| `conf/runner.toml` | Runner | **one Runner machine — all jobs it runs** |

Every key is stated as three facts: **where it is declared** → **what it takes effect on** → **who can override it**.

> Pipeline YAML parameters (triggers, steps, `config` blocks, templates, runtime env) are a different scope family — see [Pipeline Parameter Scopes](/docs/01-architecture/authoring/pipeline-parameter-scopes/).

## Minimal samples

**`manager.toml`**:

```toml
[server]
port = 8080
public_base_url = "https://buz.example.com"   # notify deep links (BUZ_JOB_URL)

[auth]
jwt_secret = "generate-with-openssl-rand-hex-32"  # or env BUZ_JWT_SECRET

marketplace_url = "https://plugins.example.com"   # Dashboard YAML-editor link
```

**`runner.toml`**:

```toml
[runner]
name = "prod-runner-01"
manager_url = "https://buz.example.com"
register_token = "paste-from-ui"              # bootstrap only
workspace_dir = "/tmp/buz-runner"
token_cache_path = "./state/runner-token"

[engine]
# docker_config_isolation = "per_job"         # default; machine-local — UI never overrides
# ssh_known_hosts_path = "~/.ssh/known_hosts"
```

## 1. Manager process — `manager.toml`

Override rule: **env vars beat TOML** (`BUZ_JWT_SECRET`, `BUZ_MARKETPLACE_URL`, `BUZ_PLUGIN_INDEX__LOCAL_DIR`). Nothing else overrides this file.

| Key | Declared in | Takes effect on | Overridable by |
|---|---|---|---|
| `marketplace_url` (`BUZ_MARKETPLACE_URL`) | `manager.toml` (top level) | Dashboard YAML-editor link | env |
| `server.host` / `server.port` | `manager.toml → [server]` | API/UI listener | — |
| `server.tls_cert` / `tls_key` | `manager.toml → [server]` | API/UI TLS | — |
| `server.public_base_url` | `manager.toml → [server]` | Notify deep links (`BUZ_JOB_URL`) | — |
| `database.path` / `max_connections` | `manager.toml → [database]` | SQLite location / pool | — |
| `auth.jwt_secret` (`BUZ_JWT_SECRET`) | `manager.toml → [auth]` | Signs JWTs **and** encrypts secrets | env |
| `auth.jwt_ttl_hours` | `manager.toml → [auth]` | UI/API session tokens | — |
| `auth.runner_register_token_ttl_hours` | `manager.toml → [auth]` | Runner registration tokens | — |
| `pipeline.yaml_scan_dir` / `yaml_scan_interval_seconds` | `manager.toml → [pipeline]` | Pipeline YAML scan | — |
| `plugin_index.local_dir` (`BUZ_PLUGIN_INDEX__LOCAL_DIR`) | `manager.toml → [plugin_index]` | `uses:` resolution | env |
| `plugin_index.reviewed_manifest` / `require_reviewed` | `manager.toml → [plugin_index]` | Template review gate | — |
| `clock.enabled` / `warn_threshold_seconds` / `cache_ttl_seconds` | `manager.toml → [clock]` | Clock probe before signing config URLs | — |
| `clock.allowed_hosts` | `manager.toml → [clock]` | SSRF allowlist for the probe — **retires with OSS delivery (Phase 3)** | — |
| `runner.heartbeat_timeout_seconds` | `manager.toml → [runner]` | Unresponsive-Runner timeout | — |
| `standalone.enabled` / `auto_approve` | `manager.toml → [standalone]` | Single-image lab mode | — |
| `logging.level` / `file` | `manager.toml → [logging]` | Manager log | — |
| `BUZ_ALLOW_INSECURE_JWT` | Manager env | Allows empty/placeholder `jwt_secret` — **dev only, never production** | — |
| `BUZ_FRONTEND_DIR` | Manager env | Serve dashboard from disk | — |

## 2. Runner machine — `runner.toml` ⟷ UI policy

Precedence: **UI policy > `runner.toml` > built-in default** — but the UI policy only governs the subset in §2.2. Everything else is machine-local.

### 2.1 Machine-local (TOML only — UI policy never touches these)

| Key | Declared in | Takes effect on | Overridable by |
|---|---|---|---|
| `name` | `runner.toml → [runner]` | UI identity of this Runner | — |
| `manager_url` | `runner.toml → [runner]` | Which Manager this Runner serves | — |
| `workspace_dir` | `runner.toml → [runner]` | Job workspace root (`{workspace_dir}/{job_id}`) | — |
| `token_cache_path` | `runner.toml → [runner]` | Durable Runner JWT cache (`0600`) | — |
| `prefer_interfaces` | `runner.toml → [runner]` | Multi-NIC IP ordering | — |
| `ssh_known_hosts_path` | `runner.toml → [engine]` | Host-key file when policy = `known_hosts` | — |
| `docker_config_isolation` | `runner.toml → [engine]` | Docker CLI config scope: `per_job` (default) / `shared` | — |
| `sandbox.supervisor_socket` / `workspace_root` / `snapshot_path` | `runner.toml → [sandbox]` | Local sandboxd Unix socket, workspace allow-root, observational snapshot | — |
| `tls_skip_verify` | `runner.toml → [runner]` | TLS verification — **dev only** | — |
| `standalone` | `runner.toml → [runner]` | Empty-token lab mode — **lab only** | — |

> **Sandbox host policy is not a config file.** Tier images, OCI runtimes, resource profiles, and the job network are Manager-managed per sandbox host (ops API / Dashboard → Sandbox Hosts); `buz-sandboxd` itself takes only host bootstrap from env. `conf/sandboxd.toml` is gone — see [Sandbox Host Configuration](/docs/01-architecture/execution/sandbox-host-configuration/) and the migration steps in [Configuration](/docs/03-operations/configuration/).

### 2.2 Policy-managed (Manager UI → Runners — overrides `runner.toml` at job time)

| Key | Declared in | Takes effect on | Overrides |
|---|---|---|---|
| `execution_mode` | UI policy (`host` default / `container` / `vm`) | Job-wide executor selected per Runner | — |
| `shell_enabled` / `ssh_enabled` / `docker_enabled` | UI policy (TOML fallback) | Engine capability of this Runner | `runner.toml → [engine]` |
| `ssh_host_key_policy` | UI policy (`known_hosts` default / `insecure`) | SSH host-key verification | `runner.toml → [engine]` |
| `max_concurrent_jobs` | UI policy | Parallel jobs on this Runner | — |
| `clean_workspace_after_job` | UI policy | Whole-workspace cleanup at job end (**default false**) | — |

`execution_mode=host` preserves current behavior. Sandboxed modes (`container`, `sandbox`, `vm`) require the local `[sandbox]` runtime for that tier and fail closed when it is unavailable; they currently reject SSH, per-step Docker, and host-side config preparation. The local socket/image/runtime paths constrain the UI-selected mode and are never weakened by Manager policy. See [Job-wide execution modes](/docs/01-architecture/execution/runner-concurrent-jobs/#job-wide-execution-modes).

Job-scoped Docker credentials (`.docker` in the job workspace) are removed **unconditionally** at job end regardless of `clean_workspace_after_job`.

### 2.3 Bootstrap-only

| Key | Declared in | Takes effect on | Notes |
|---|---|---|---|
| `register_token` (`BUZ_REGISTER_TOKEN`) | `runner.toml → [runner]` | First registration only | reissue via Manager UI |
| `[clock] enabled` / `fail_on_skew` / `warn_threshold_seconds` / `max_skew_seconds` | `runner.toml → [clock]` | Startup clock check | — |

## Related

- Pipeline YAML parameters (triggers, steps, `config` block, templates, `BUZ_*` runtime env): [Pipeline Parameter Scopes](/docs/01-architecture/authoring/pipeline-parameter-scopes/)
- Full option reference: [Configuration Reference](/docs/03-operations/configuration/)
- Runner strategy (concurrency, engines, cleanup): [Runner Policy](/docs/01-architecture/execution/runner-concurrent-jobs/)
