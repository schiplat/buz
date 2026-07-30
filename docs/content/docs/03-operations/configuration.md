---
title: Configuration Reference
weight: 2
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
jwt_secret = "changeme-demo-only"   # or env BUZ_JWT_SECRET
jwt_ttl_hours = 24
runner_register_token_ttl_hours = 24

[runner]
heartbeat_timeout_seconds = 90

[pipeline]
yaml_scan_dir = "./pipelines"
yaml_scan_interval_seconds = 30
```

| Section | Key | Notes |
|---------|-----|-------|
| `server` | `host` / `port` | Listen address |
| `server` | `tls_cert` + `tls_key` | Optional HTTPS (both required). See [TLS](/docs/03-operations/tls/) |
| `database` | `path` | SQLite file |
| `auth` | `jwt_secret` | Signs JWTs + encrypts host credentials |
| `auth` | `jwt_ttl_hours` | UI/API JWT lifetime |
| `auth` | `runner_register_token_ttl_hours` | Runner JWT lifetime after register |
| `runner` | `heartbeat_timeout_seconds` | Offline threshold for runners |
| `pipeline` | `yaml_scan_*` | Directory sync of pipeline YAML |

**First admin:** not in config. On a fresh database, open the UI — you are redirected to `/setup` to create the platform admin (password ≥ 8 chars, complexity rules apply). After that, use `/login`.

**Password storage:** login passwords use **Argon2id** in `users.password_hash`. This is separate from Secrets/Hosts **AES-256-GCM** encryption. A login failure is almost always a wrong password (or wrong username), not a crypto mismatch.

**Forgot admin password (local/dev recovery):** stop Manager, then from a machine with the Argon2 tooling available set a new hash, e.g. generate one via a one-liner and:

```sql
UPDATE users SET password_hash = '<argon2id-phc-string>' WHERE username = 'admin';
```

Then restart Manager and sign in. Prefer changing the password again from the UI after recovery.

## Runner (`conf/runner.toml`)

Machine-local only. Strategy is **not** configured here — see [Runner Policy](/docs/01-architecture/runner-concurrent-jobs/).

| Managed in UI | Kept in TOML |
|---------------|--------------|
| `labels` | `manager.url`, `register_token`, `tls_skip_verify` |
| `max_concurrent_jobs`, cleanup (`clean_workspace_after_job`) | `runner.name`, `workspace_dir` |
| engine enable flags + `ssh_host_key_policy` | `prefer_interfaces`, `ssh_known_hosts_path` |

```toml
[manager]
url = "https://buz-api.example.com"
register_token = "paste-from-ui"   # or env BUZ_REGISTER_TOKEN
# tls_skip_verify = false

[runner]
name = "prod-runner-01"            # must match UI Runner name
workspace_dir = "/tmp/buz-runner"
# prefer_interfaces = ["enp26s0"]

[engine]
# ssh_known_hosts_path = "/home/runner/.ssh/known_hosts"
```

## Environment overrides

| Variable | Effect |
|----------|--------|
| `BUZ_JWT_SECRET` | Overrides `auth.jwt_secret` |
| `BUZ_MANAGER_URL` | Overrides `manager.url` |
| `BUZ_REGISTER_TOKEN` | Overrides `manager.register_token` |
| `BUZ__…` | Nested TOML via `config` crate (`BUZ__SERVER__PORT=8081`) |

Priority: CLI args > environment > config file > defaults.

> There is **no** `env.VAR_NAME` interpolation inside TOML strings. Use the variables above (or `BUZ__` nested keys).
