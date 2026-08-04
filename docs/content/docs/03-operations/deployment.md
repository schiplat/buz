---
title: Deployment
weight: 2
description: "Deploying Center and Runner in production"
---

> **New here?** Start with [Quick Start](/docs/00-quick-start/) (`make run`). This page covers **production** deploy: release packages, split Manager + Runner, Docker, and upgrades.
>
> **Lab / single-VM?** Prefer the one-container image — see [Standalone](/docs/03-operations/standalone/) (`buz-standalone`, arm64).

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
git tag v1.3.0 && git push origin v1.3.0
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
| `buz-standalone` (arm64 only) | `Dockerfile.standalone` — Manager + Runner in one image |

Both split images are built for `linux/amd64` **and** `linux/arm64` (native matrix builds: `ubuntu-latest` for amd64, `ubuntu-24.04-arm` for arm64, no QEMU emulation). The matrix jobs push platform-specific tags (`v1.3.0-amd64` / `v1.3.0-arm64`), and a final `merge` job assembles the shared multi-arch tags (`v1.3.0` / `1.3.0` / `latest`) with `docker buildx imagetools create`. Pushing the same tag concurrently from both jobs would make ACR produce a broken index (arm64 as `unknown/unknown`), so the two tags are never pushed in parallel.

The standalone image is **arm64-only** (workflow `.github/workflows/docker-standalone.yml` on `ubuntu-24.04-arm`); tags are `buz-standalone:vX.Y.Z` / `:latest` / `:X.Y.Z` plus `-arm64` suffixes.

**Standalone auth (dev only).** The image enables Manager `[standalone]` so the
co-located Runner may register over loopback without `register_token`, but only
if the runner name is already authorized (UI → New Runner, or
`standalone.auto_approve`). Non-loopback registration is rejected. This does
**not** change the production register_token protocol used by split
`buz-manager` / `buz-runner` deployments.

**Full first-run and upgrade guide:** [Standalone (single image)](/docs/03-operations/standalone/).
Template Compose: `deploy/standalone/docker-compose.yml`.

### GitHub Actions → personal ACR

Workflows:
- `.github/workflows/docker.yml` — split `buz-manager` / `buz-runner` (amd64 + arm64)
- `.github/workflows/docker-standalone.yml` — combined `buz-standalone` (**arm64 only**)

Both trigger on `v*` tags or manual `workflow_dispatch`.

Configure the GitHub repository:

| Kind | Name | Example |
|------|------|---------|
| Variable | `ACR_REGISTRY` | `registry.cn-hangzhou.aliyuncs.com` |
| Variable | `ACR_NAMESPACE` | your ACR namespace |
| Secret | `ACR_USERNAME` | ACR login user |
| Secret | `ACR_PASSWORD` | ACR password / token |

Pushed tags: `<registry>/<namespace>/buz-manager:v1.3.0` (and `:1.3.0`, `:latest`), same for `buz-runner`. Standalone: `<registry>/<namespace>/buz-standalone:v1.3.0` (linux/arm64).

### Container layout

Each image contains **only** the binary + a default config (UI is embedded in `buz-manager`):

```text
/app/buz/
  buz-manager | buz-runner          # standalone image has both + entrypoint.sh
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

### One-shot VM deploy (`deploy/dev-deploy.sh`)

For a fresh Linux VM (e.g. a Linux VM on macOS, arm64 or x86_64) that only has Docker:

```bash
./deploy/dev-deploy.sh login   # docker login to ACR (or set ACR_USERNAME/ACR_PASSWORD)
./deploy/dev-deploy.sh up      # pull images + start Manager on :8080
# 1st: open the web UI (http://<vm>:8080) and complete /setup to create the platform admin
# Then bootstrap the Runner on the same VM:
ADMIN_USERNAME='<admin>' ADMIN_PASSWORD='<password>' ./deploy/dev-deploy.sh runner
./deploy/dev-deploy.sh status  # container + health
./deploy/dev-deploy.sh logs    # follow logs
./deploy/dev-deploy.sh down    # stop (volumes kept)
```

All knobs are env-overridable: `ACR_REGISTRY`, `ACR_NAMESPACE`, `IMAGE_TAG`,
`MANAGER_PORT`, `DEPLOY_DIR` (default `/opt/buz`), `RUNNER_NAME`, `RUNNER_TOKEN`,
`DOCKER`, `BUZ_CONTAINER_UID`.
A `BUZ_JWT_SECRET` is generated on first `up` and persisted in `$DEPLOY_DIR/.env`
so stored secrets stay decryptable across restarts. `render` writes the compose
file + runner config without touching Docker (useful to preview before deploying).

**Works on both arm64 and amd64 VMs.** The default `IMAGE_TAG=latest` points to
the merged multi-arch manifest, so Docker pulls the image matching the VM's
architecture automatically. To pin a single platform — for example while the CI
`merge` job has not finished yet, or to bypass a broken index — set a
platform-specific tag instead: `IMAGE_TAG=latest-arm64` (or `latest-amd64` /
`1.3.0-arm64`). Verify the platform tags with
`docker manifest inspect <registry>/<namespace>/buz-manager:1.3.0-arm64`.

**Runs as a non-root user with sudo.** Every `docker` invocation is prefixed with
`sudo` when the current user cannot reach the daemon directly (auto-detected;
override with `DOCKER='sudo docker'`, or add your user to the docker group).
`$DEPLOY_DIR` is created with sudo when needed and owned by the caller. The buz
containers run as `USER buz` (uid `BUZ_CONTAINER_UID`, default 10001), so the
bind-mounted data dirs (`data`, `pipelines`, `workspace`, `logs`) are chown'ed to
that uid — this keeps them writable by the containers even though the host user
is not root. `conf/` stays owned by the deploying user so `runner.toml` can be
(re)written by the script.

**Troubleshooting: Runner stuck in a restart loop with `Cannot reach Buz Manager`.**
If `docker logs buz-runner` shows `error sending request for url (http://manager:8080/...)`
while `docker ps` shows `buz-manager` is `Up (healthy)`, usual causes are:
(1) Runner started before Manager was ready, or (2) compose DNS alias `manager`
went stale after Manager was recreated.

One-shot fix on a `dev-deploy.sh` host:

```bash
# Prefer upgrade (waits for Manager health, recreates Runner after sleep 3s).
# If the stack was first created with sudo/root, keep using the same daemon:
sudo SKIP_PULL=1 IMAGE_TAG=<current-or-new-tag> ./dev-deploy.sh upgrade
# or:
DOCKER='sudo docker' SKIP_PULL=1 IMAGE_TAG=<tag> ./dev-deploy.sh upgrade
# Or just repair Runner:
sudo SKIP_PULL=1 ./dev-deploy.sh runner
```

Mixing plain `docker` (docker group / rootless) with `sudo docker` (rootful) talks
to different daemons — Runner recreated on the wrong side never reaches Manager.
`dev-deploy.sh` prefers the daemon that already has `buz-manager`.

Manual check: `sudo docker exec buz-runner getent hosts manager` (should return the
Manager container IP) and compare against
`sudo docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' buz-manager`.
Same-compose default is container↔container: `manager_url = "http://manager:8080"`
with `depends_on: manager: condition: service_healthy`.

**Runner provisioning requires a platform admin.** `runner` auto-creates the
runner by calling `buz-manager gen-runner-token` inside the Manager container.
That CLI subcommand verifies the caller holds a platform-admin credential —
either a login JWT or an **API token** (created in Manager UI → Access → API
tokens; recommended for automation since it is revocable and audited via
`api_token.*` events) — before touching the `runners` table. It logs in via
`POST /api/v1/auth/login` using `ADMIN_USERNAME`/`ADMIN_PASSWORD`, or accepts a
pre-obtained `BUZ_ADMIN_TOKEN`. To provision from elsewhere, export the token
yourself:

```bash
# Option A: login JWT
ADMIN_TOKEN=$(curl -s -X POST http://127.0.0.1:8080/api/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"<admin>","password":"<password>"}' | jq -r .data.token)
# Non-root user: use `sudo docker` (or whatever DOCKER resolves to)
docker exec buz-manager /app/buz/buz-manager -c /app/buz/conf/manager.toml \
  gen-runner-token linux-vm-runner --labels linux-x64 --token "$ADMIN_TOKEN"
# name: linux-vm-runner
# register_token: <one-shot token>   → paste into conf/runner.toml

# Option B (preferred for scripts): API token from Access → API tokens
BUZ_ADMIN_TOKEN='buz_...' docker exec buz-manager \
  /app/buz/buz-manager -c /app/buz/conf/manager.toml \
  gen-runner-token linux-vm-runner --labels linux-x64
```

## Reverse Proxy (Nginx, HTTP-only)

`buz-manager` serves both the API and the embedded Web UI on a single port. Put Nginx in front so only the proxy is exposed publicly.

Templates:
- `deploy/nginx/buz-manager.conf` — manager running directly on the host (HTTP)
- `deploy/nginx/buz-manager-docker.conf` — manager started via `deploy/dev-deploy.sh` (Docker, HTTP)
- `deploy/nginx/buz-manager-ssl.conf` — HTTPS + redirect

### Docker deployment (`dev-deploy.sh`)

```bash
# 1) Tighten the published port so only Nginx is reachable from outside:
#    in /opt/buz/docker-compose.yml change
#        - "${MANAGER_PORT}:8080"
#    to
#        - "127.0.0.1:${MANAGER_PORT}:8080"
#    then `./dev-deploy.sh down && ./dev-deploy.sh up` (JWT secret + volumes kept).

# 2) Install the Nginx site config (non-root user with sudo)
sudo cp deploy/nginx/buz-manager-docker.conf /etc/nginx/sites-available/buz-manager.conf
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

> Enable Docker steps (`uses: docker`) in **Manager UI → Runners → Docker**. See [Docker Engine](/docs/01-architecture/execution/docker-engine/). Host-side `docker build` / SSH `docker …` does **not** need this flag.

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
