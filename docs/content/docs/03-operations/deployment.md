---
title: Deployment
weight: 1
description: "Deploying Center and Runner in production"
---

## Quick Start

```bash
# One-step: build all + run Center
make run
# → http://localhost:8080  (first visit → /setup to create admin)

# Or step by step
make example        # create example config directory
make build          # build frontend + backend
cd example && ../target/release/buz-manager
```

## Embedded Web UI

`buz-manager` embeds `frontend/dist` at compile time (`rust-embed`). Production binaries and Docker images need **no** separate `frontend/` directory.

```bash
make build-frontend   # required before release cargo build
make build-backend
./target/release/buz-manager -c conf/manager.toml
# Optional disk override for UI iteration: -f frontend/dist
```

## GitHub Release packages

CI workflow `.github/workflows/release.yml` builds **Linux + macOS** archives (UI embedded in `buz-manager`) on tag `v*`:

| Archive | Runner |
|---------|--------|
| `buz-<ver>-linux-x86_64.tar.gz` | ubuntu-latest |
| `buz-<ver>-linux-aarch64.tar.gz` | ubuntu-24.04-arm |
| `buz-<ver>-macos-aarch64.tar.gz` | macos-14 (Apple Silicon) |
| ~~`buz-<ver>-macos-x86_64.tar.gz`~~ | temporarily disabled (macos-13 / Intel) |

```text
buz-<version>-linux-x86_64.tar.gz
  bin/buz-manager   # UI embedded
  bin/buz-runner
  conf/manager.toml
  conf/runner.toml
  data/  pipelines/
  README.md
```

```bash
git tag v1.1.0 && git push origin v1.1.0
make package   # local host only → dist/release/buz-*.tar.gz
```

```bash
tar -xzf buz-*-linux-x86_64.tar.gz && cd buz-*
# edit conf/*.toml
./bin/buz-manager -c conf/manager.toml
```

Apple Silicon Mac: use `macos-aarch64`. Intel Mac (`macos-x86_64`) builds are temporarily disabled in CI.
## Docker (Aliyun ACR)

Images:

| Image | Dockerfile |
|-------|------------|
| `buz-manager` | `Dockerfile` |
| `buz-runner` | `Dockerfile.runner` |

Both images are built for `linux/amd64` **and** `linux/arm64` (native matrix builds: `ubuntu-latest` for amd64, `ubuntu-24.04-arm` for arm64, no QEMU emulation).

### GitHub Actions → personal ACR

Workflow: `.github/workflows/docker.yml` (on `v*` tag or manual).

Configure the GitHub repository:

| Kind | Name | Example |
|------|------|---------|
| Variable | `ACR_REGISTRY` | `registry.cn-hangzhou.aliyuncs.com` |
| Variable | `ACR_NAMESPACE` | your ACR namespace |
| Secret | `ACR_USERNAME` | ACR login user |
| Secret | `ACR_PASSWORD` | ACR password / token |

Pushed tags: `<registry>/<namespace>/buz-manager:v1.1.0` (and `:1.1.0`, `:latest`), same for `buz-runner`.

### Container layout

Each image contains **only** the binary + a default config (UI is embedded in `buz-manager`):

```text
/app/buz/
  buz-manager | buz-runner
  conf/manager.toml | conf/runner.toml
  data/  pipelines/          # Manager volumes
  workspace/                 # Runner volume
```

Host templates for Compose **and** tar.gz releases live in **`deploy/conf/`**. Compose mounts them into `/app/buz/conf/…` (`BUZ_MANAGER_CONF` / `BUZ_RUNNER_CONF`).

### Deploy modes

| Mode | How |
|------|-----|
| Same host | Manager → UI create Runner → edit `deploy/conf/runner.toml` → start Runner |
| Manager only | `docker compose up -d manager` |
| Runner elsewhere | `BUZ_RUNNER_CONF=/etc/buz/runner.toml docker compose up -d runner` |

```bash
docker build -t buz-manager:local -f Dockerfile .
docker build -t buz-runner:local -f Dockerfile.runner .

export BUZ_JWT_SECRET="$(openssl rand -hex 32)"
docker compose up -d manager
# UI → Runners → New Runner → paste token into deploy/conf/runner.toml
docker compose up -d runner

# Other machine:
export BUZ_RUNNER_CONF=/etc/buz/runner.toml   # manager_url = Center URL
docker compose up -d runner
```

Pull from ACR:

```bash
docker login registry.cn-hangzhou.aliyuncs.com
docker pull registry.cn-hangzhou.aliyuncs.com/<ns>/buz-manager:latest
docker pull registry.cn-hangzhou.aliyuncs.com/<ns>/buz-runner:latest
```

## Reverse Proxy (Nginx, HTTP-only)

`buz-manager` serves both the API and the embedded Web UI on a single port. Put Nginx in front so only the proxy is exposed publicly.

Template: `deploy/nginx/buz-manager.conf` (HTTP-only) or `deploy/nginx/buz-manager-ssl.conf` (HTTPS + redirect)

```bash
# 1) Bind Manager to loopback only
sudo tee -a conf/manager.toml >/dev/null <<'EOF'
[server]
host = "127.0.0.1"
port = 8080
EOF

# 2) Install the Nginx site config (replace server_name first)
sudo cp deploy/nginx/buz-manager.conf /etc/nginx/sites-available/buz-manager.conf
sudo ln -s ../sites-available/buz-manager.conf /etc/nginx/sites-enabled/buz-manager.conf
sudo nginx -t && sudo systemctl reload nginx
```

The config covers:

- WebSocket upgrade for live job logs (`/api/v1/jobs/{id}/logs/stream`) and pipeline stream
- Runner long-poll (50s) + WS keepalive timeouts
- `client_max_body_size` for pipeline YAML import and log uploads
- gzip for embedded UI assets and JSON

Runners behind the proxy:

```toml
[runner]
manager_url = "http://buz.example.com"   # no :8080; proxy forwards to Manager
```

For HTTPS, add `listen 443 ssl` + certificate lines to the site config (see [HTTPS Communication](/docs/03-operations/tls/)).

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
# Prefer env: export BUZ_JWT_SECRET="$(openssl rand -hex 32)"
jwt_secret = ""

[runner]
heartbeat_timeout_seconds = 90

[pipeline]
yaml_scan_dir = "./pipelines"
yaml_scan_interval_seconds = 30
EOF

# Start
export BUZ_JWT_SECRET="$(openssl rand -hex 32)"
./target/release/buz-manager
# Open https://… or http://… → /setup to create the first admin
```

For HTTPS (Manager native TLS, Runner client, reverse proxy, full commands), see **[HTTPS Communication](/docs/03-operations/tls/)**.

## Runner Deployment

Runners can be deployed on any machine with outbound HTTPS access to the Center.

```bash
# Create config
mkdir -p conf
cat > conf/runner.toml <<EOF
[runner]
name = "prod-runner-01"
manager_url = "https://buz-api.example.com"
register_token = "your-register-token"
workspace_dir = "/tmp/buz-runner"

[engine]
# ssh_known_hosts_path = "/home/runner/.ssh/known_hosts"

[logging]
level = "info"
EOF

export BUZ_REGISTER_TOKEN="your-token"
./target/release/buz-runner
```

> Enable Docker steps (`uses: docker`) in **Manager UI → Runners → Docker**. See [Docker Engine](/docs/01-architecture/docker-engine/). Host-side `docker build` / SSH `docker …` does **not** need this flag.

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
7. Runner labels in Manager UI must include `linux-x64` (match YAML `runner.labels`).

YAML team assignment:

```yaml
team: default   # or another existing team slug; omit → default
```

### SSH host keys (production)

Default UI policy is `insecure` (accept any server key) for lab convenience. For production, set **SSH host key** to `known_hosts` in Manager UI → Runners. Optionally point the local file in `runner.toml`:

```toml
[engine]
ssh_known_hosts_path = "/home/runner/.ssh/known_hosts"  # optional; default ~/.ssh/known_hosts
```

Pre-seed keys with `ssh-keyscan -p 22 host >> ~/.ssh/known_hosts`. A changed host key fails the step with an explicit error.
