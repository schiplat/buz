---
title: Configuration Reference
weight: 2
description: "All configuration options for Manager and Runner"
---

## Manager (`conf/manager.toml`)

```toml
[server]
host = "0.0.0.0"           # Bind address
port = 8080                # Listen port

[database]
path = "./data/buz.db"     # SQLite file path
max_connections = 8        # Connection pool size

[auth]
jwt_secret = "env.BUZ_JWT_SECRET"
admin_user = "admin"
admin_password_hash = "env.BUZ_ADMIN_PASSWORD_HASH"
runner_register_token_ttl_hours = 24
jwt_ttl_hours = 24

[runner]
heartbeat_timeout_seconds = 90
long_poll_timeout_seconds = 45

[pipeline]
yaml_scan_dir = "./pipelines"
yaml_scan_interval_seconds = 30
```

## Runner (`conf/runner.toml`)

```toml
[manager]
url = "https://buz-api.example.com"
register_token = "env.BUZ_REGISTER_TOKEN"

[runner]
name = "prod-runner-01"
labels = ["prod", "linux-x64"]
workspace_dir = "/tmp/buz-runner"
# prefer_interfaces = ["enp26s0"]  # multi-NIC: advertise these IPs first in UI

[engine]
shell_enabled = true
ssh_enabled = true
docker_enabled = false  # MVP: set true for uses: docker — docs/01-architecture/docker-engine.md
wasm_enabled = false    # Phase 3
# insecure (default) | known_hosts
ssh_host_key_policy = "known_hosts"
# ssh_known_hosts_path = "/home/runner/.ssh/known_hosts"  # default: ~/.ssh/known_hosts
```

| Key | Default | Notes |
|-----|---------|-------|
| `runner.prefer_interfaces` | `[]` | NIC names whose IPs are listed first in Runners UI; if empty, prefer IPs on the same subnet as `manager.url` |
| `engine.ssh_host_key_policy` | `insecure` | `known_hosts` rejects unknown/changed server keys |
| `engine.ssh_known_hosts_path` | `~/.ssh/known_hosts` | Used when policy is `known_hosts` |

> Runner identity lives under TOML `[runner]`. Env overrides use `BUZ__RUNNER__*` (e.g. `BUZ__RUNNER__NAME`). Manager’s own `[runner]` section (heartbeat / long-poll timeouts) is unrelated — it only appears in `manager.toml`.

## Environment Variables

Sensitive values with the `env.VAR_NAME` pattern in config files are resolved at startup. Values provided via environment variables take precedence over config file defaults.

Priority: CLI args > environment variables > config file > defaults.
