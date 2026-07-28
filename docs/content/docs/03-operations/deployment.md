---
title: Deployment
weight: 1
description: "Deploying Center and Runner in production"
---

## Quick Start

```bash
# One-step: build all + run Center
make run
# → http://localhost:8080  (admin / admin)

# Or step by step
make example        # create example config directory
make build          # build frontend + backend
cd example && ../target/release/buz-manager
```

## Center Deployment

The Center binary includes the frontend static assets and SQLite database. No external dependencies required.

```bash
# Build
make build-frontend
make build-backend

# Create config (or use `make example`)
mkdir -p conf
cat > conf/manager.toml <<'EOF'
[server]
host = "0.0.0.0"
port = 8080

[database]
path = "./data/buz.db"

[auth]
jwt_secret = "changeme-demo-only"
admin_user = "admin"
admin_password_hash = "admin"
EOF

# Start
export BUZ_JWT_SECRET="$(openssl rand -hex 32)"
./target/release/buz-manager
```

## Runner Deployment

Runners can be deployed on any machine with outbound HTTPS access to the Center.

```bash
# Create config
mkdir -p conf
cat > conf/runner.toml <<EOF
[center]
url = "https://buz-api.example.com"
register_token = "your-register-token"

[node]
name = "prod-node-01"
labels = ["prod", "linux-x64"]
work_dir = "/tmp/buz-runner"

[engine]
local_enabled = true
ssh_enabled = true
docker_enabled = false
wasm_enabled = false
EOF

export BUZ_REGISTER_TOKEN="your-token"
./target/release/buz-runner
```

> `docker_enabled` is reserved for the step Docker engine ([design](/docs/01-architecture/docker-engine/)). Host-side `docker build` / SSH `docker …` does **not** need this flag.

## Requirements

| Component | Minimum |
|-----------|---------|
| Runner memory (idle) | < 20MB |
| Center memory (idle) | < 50MB |
| Network | Runner → Center outbound HTTPS only |
| Database | SQLite (built-in, zero setup) |

## Troubleshooting: sidebar clicks dead after Manager restart

**Symptom:** After stopping and restarting Manager, sidebar links often do nothing on click, but the same `href` still works via address bar / new tab.

**Cause:** While Manager is down, in-flight API polls / hung requests occupy the browser per-host connection pool. SPA route lazy-imports share that pool, so `router.push` hangs. Full page loads use a fresh context and still work.

**Mitigations in the UI (already shipped):**

- Sidebar uses `safeNavigate` — if SPA navigation does not settle within ~4s, it hard-loads the target URL.
- Job log polling skips overlapping requests and aborts on unmount.
- Job / pipeline WebSockets reconnect with backoff after Manager restart.
- Vite API proxy enables `ws: true` for log streams.

**Debug logging (browser console):**

```js
localStorage.setItem('buz_debug_nav', '1')
location.reload()
```

Then click sidebar items. Look for `[buz-nav]` / `[buz]` lines:

| Log | Meaning |
|-----|---------|
| `sidebar click` / `start` | Click reached the nav handler |
| `ok` | SPA navigation finished |
| `navigation timed out — hard fallback` | Connection pool / chunk load hung; full reload used |
| `api request` / `inFlight` | How many HTTP calls are outstanding |
| `router.onError` | Chunk / dynamic import failure |

Disable with `localStorage.removeItem('buz_debug_nav')`.

---

## End-to-end smoke (Phase 1 + Team)

Minimal path after `make example` / `make build`:

1. Start Manager + Runner (`example/conf/*.toml`). Login with configured admin (re-login after RBAC upgrade).
2. Confirm **Access → Teams** has `default` (created at bootstrap).
3. **Hosts**: register `dev-vm-1` with SSH key / password / agent.
4. Copy `example/pipelines/ssh-verify.yml` (and optionally `smoke-test.yml`) into the Manager `yaml_scan_dir`. Wait for scanner (~interval) or restart Manager.
5. Pipelines list should show the YAML pipelines under team `default`.
6. Trigger **SSH Verify**; open Job Detail — steps + live logs should stream.
7. Runner labels must include `linux-x64` (match YAML `runner.labels`).

YAML team assignment:

```yaml
team: default   # or another existing team slug; omit → default
```

### SSH host keys (production)

Default policy is `insecure` (accept any server key) for lab convenience. For production runners:

```toml
[engine]
ssh_host_key_policy = "known_hosts"
# ssh_known_hosts_path = "/home/runner/.ssh/known_hosts"  # optional; default ~/.ssh/known_hosts
```

Pre-seed keys with `ssh-keyscan -p 22 host >> ~/.ssh/known_hosts`. A changed host key fails the step with an explicit error.
