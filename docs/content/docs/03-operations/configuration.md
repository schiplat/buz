---
title: Configuration Reference
weight: 1
description: "All configuration options for Manager and Runner"
---

## Manager (`conf/manager.toml`)

```toml
[server]
host = "0.0.0.0"
port = 8080
# tls_cert = "certs/cert.pem"   # both set → HTTPS
# tls_key  = "certs/key.pem"

[database]
path = "./data/buz.db"
max_connections = 8

[auth]
jwt_secret = ""                     # required — or set BUZ_JWT_SECRET (openssl rand -hex 32)
# Local/demo with a weak template secret only: BUZ_ALLOW_INSECURE_JWT=1
jwt_ttl_hours = 24
runner_register_token_ttl_hours = 24

[runner]
heartbeat_timeout_seconds = 90

[pipeline]
yaml_scan_dir = "./pipelines"
yaml_scan_interval_seconds = 30

# Dev / single-container only — leave disabled in production (default)
# [standalone]
# enabled = true
# auto_approve = ["standalone-runner"]

[logging]
level = "info"                 # error | warn | info | debug | trace
# file = "./logs/manager.log"  # optional; when set → stdout + file
```

| Section | Key | Notes |
|---------|-----|-------|
| `server` | `host` / `port` | Listen address |
| `server` | `tls_cert` + `tls_key` | Optional HTTPS (both required). See [TLS](/docs/03-operations/tls/) |
| `database` | `path` | SQLite file |
| `auth` | `jwt_secret` | Signs JWTs + encrypts secrets/hosts. **Required** (≥ 32 chars). Empty / `CHANGE_ME` / `changeme` / `demo-only` placeholders are **refused at startup** unless `BUZ_ALLOW_INSECURE_JWT=1` (local only) |
| `auth` | `jwt_ttl_hours` | UI/API JWT lifetime |
| `auth` | `runner_register_token_ttl_hours` | Runner JWT lifetime after register |
| `runner` | `heartbeat_timeout_seconds` | Offline threshold for runners |
| `pipeline` | `yaml_scan_*` | File/Git authority for pipeline YAML; scanner + UI online edit write here. **Flat directory only** (basename files; no private subdirs). See [YAML-First Authoring](/docs/01-architecture/authoring/yaml-first-authoring/) |
| `standalone` | `enabled` | Default `false`. When `true`, allow **loopback** Runner register without `register_token` for names in `auto_approve` (or pre-created in UI). Non-loopback register is **rejected**. Dev / `buz-standalone` only — see [Standalone mode](#standalone-mode) |
| `standalone` | `auto_approve` | Runner names created at Manager boot if missing. Default `["standalone-runner"]` |
| `logging` | `level` | Default `info`. Overridden by `RUST_LOG` if set |
| `logging` | `file` | Optional append path; empty = stdout only |

**First admin:** not in config. On a fresh database, open the UI — you are redirected to `/setup` to create the platform admin (password ≥ 8 chars, complexity rules apply). After that, use `/login`.

**Password storage:** login passwords use **Argon2id** in `users.password_hash`. This is separate from Secrets/Hosts **AES-256-GCM** encryption. A login failure is almost always a wrong password (or wrong username), not a crypto mismatch.

**Forgot admin password (local/dev recovery):** stop Manager, then from a machine with the Argon2 tooling available set a new hash, e.g. generate one via a one-liner and:

```sql
UPDATE users SET password_hash = '<argon2id-phc-string>' WHERE username = 'admin';
```

Then restart Manager and sign in. Prefer changing the password again from the UI after recovery.

## Runner (`conf/runner.toml`)

Machine-local only. Strategy is **not** configured here — see [Runner Policy](/docs/01-architecture/execution/runner-concurrent-jobs/).

| Managed in UI | Kept in TOML (`[runner]` / `[engine]` / `[logging]`) |
|---------------|-----------------------------------------------------|
| `labels` | `manager_url`, `register_token`, `tls_skip_verify`, `standalone` |
| `max_concurrent_jobs`, cleanup | `name`, `workspace_dir` |
| engine enable flags + `ssh_host_key_policy` | `prefer_interfaces`, `ssh_known_hosts_path` |

```toml
[runner]
name = "prod-runner-01"              # must match UI Runner name
manager_url = "https://buz-api.example.com"
register_token = "paste-from-ui"     # or env BUZ_REGISTER_TOKEN
# standalone = false                 # true → allow empty register_token (lab image only)
# tls_skip_verify = false
workspace_dir = "/tmp/buz-runner"
# prefer_interfaces = ["enp26s0"]

[engine]
# ssh_known_hosts_path = "/home/runner/.ssh/known_hosts"

[logging]
level = "info"
# file = "./logs/runner.log"   # Runner default if omitted: ./logs/runner.log
```

| Key | Notes |
|-----|-------|
| `register_token` | Required unless `standalone = true` (see [Standalone mode](#standalone-mode)) |
| `standalone` | Default `false`. When `true`, Runner may start with an empty `register_token` and register over loopback against a Manager with `[standalone] enabled = true` |

> Obsolete `[manager]` section is **rejected** at startup with a migration hint.

## Standalone mode

**Dev / lab only** — same-container Manager + Runner (`buz-standalone` image), or a local loopback pair. Do **not** enable on a production multi-runner control plane.

### Manager — `[standalone]`

```toml
[standalone]
enabled = true
auto_approve = ["standalone-runner"]   # created at boot if missing
```

| Behavior | Detail |
|----------|--------|
| Boot | For each name in `auto_approve`, create an authorized runner row if absent (`labels` include `standalone`) |
| Register (loopback) | Peer `127.0.0.1` / `::1` may register **without** `register_token` when the runner name is authorized (`auto_approve` or UI **New Runner**) |
| Register (remote) | Always **rejected** while `enabled = true` — even with a valid token |
| Default | `enabled = false` — normal token registration only |

Checked-in template: `deploy/conf/manager.standalone.toml`.

### Runner — `[runner].standalone`

```toml
[runner]
name = "standalone-runner"           # must match auto_approve / UI name
manager_url = "http://127.0.0.1:8080"
standalone = true
register_token = ""                  # allowed when standalone = true
```

| Behavior | Detail |
|----------|--------|
| Startup | Empty `register_token` is allowed only when `standalone = true`; otherwise Manager refuses to start the process |
| Pairing | Expects Manager `[standalone] enabled = true` and loopback `manager_url` |

Checked-in template: `deploy/conf/runner.standalone.toml`.

First-run Compose, volumes, and upgrade: **[Standalone (single image)](/docs/03-operations/standalone/)**.

## Logging

| Destination | Behavior |
|-------------|----------|
| stdout | Always on (Docker: `docker logs`) |
| Runner file | **Default** `./logs/runner.log` (Docker WORKDIR → `/app/buz/logs/runner.log`) |
| Manager file | Optional via `logging.file` (no default file) |
| Level | `logging.level` (default `info`); `RUST_LOG` overrides |

Set `logging.file = ""` (or `"-"` / `"none"`) on Runner for **stdout-only**.

Examples:

```bash
# Config file
# [logging]
# level = "debug"
# file = "/app/buz/logs/manager.log"

# One-shot override
RUST_LOG=buz_manager=debug,sqlx=warn ./buz-manager -c conf/manager.toml
```

Do **not** log secrets (`register_token`, JWT, passwords). Create-runner API returns the token once in the JSON body; process logs only record name/id.

## Environment overrides

| Variable | Effect |
|----------|--------|
| `BUZ_JWT_SECRET` | Overrides `auth.jwt_secret` (required for packaged/Docker deploys) |
| `BUZ_ALLOW_INSECURE_JWT` | `1` / `true` — allow empty or placeholder JWT secrets (**local/demo only**) |
| `BUZ_MANAGER_URL` | Overrides `runner.manager_url` |
| `BUZ_REGISTER_TOKEN` | Overrides `runner.register_token` |
| `RUST_LOG` | Overrides `logging.level` filter |
| `BUZ__…` | Nested TOML via `config` crate (`BUZ__SERVER__PORT=8081`) |

Priority: CLI args > environment > config file > defaults.

> There is **no** `env.VAR_NAME` interpolation inside TOML strings. Use the variables above (or `BUZ__` nested keys).
