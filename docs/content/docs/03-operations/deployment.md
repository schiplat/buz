---
title: Deployment
weight: 20
description: "Production deployment - container images or release binaries"
---

> **Lab / single box?** The [Quick Start](/docs/00-quick-start/) demo and the
> [Standalone](/docs/03-operations/standalone/) image run everything in one
> container.
> **Building from source?** See [Development - Build from
> source](/docs/02-development/build-from-source/). This page covers
> deployment **from published artifacts only** - no compilation.

Two supported deployment modes:

| Mode | What you deploy | Artifacts |
|------|-----------------|-----------|
| **Container** | `buz-manager` / `buz-runner` / `buz-standalone` images | Multi-arch (amd64 + arm64), Aliyun ACR + Docker Hub |
| **Binary** | Release archives with `buz-manager` + `buz-runner` (dashboard embedded) | GitHub Releases (linux-x86_64 / linux-aarch64 / macos-aarch64) |

Both consume the same config files (`manager.toml` / `runner.toml`), the same
`BUZ_JWT_SECRET` derivation, and the same SQLite storage - switching between
them does not change runtime behavior.

{{< callout type="warning" >}}
**Run exactly one Manager process per SQLite database.** The background
schedulers (queue dispatch, cron triggers, GitHub fetch) elect a leader via
the `scheduler_lease` table, so a second process would not double-fire jobs —
but the Runner wake-up bus and the WebSocket log/status broadcast are
in-process: a second Manager would serve stale real-time views and its
Runners would fall back to polling timeouts. High availability at the Manager
tier is a backup/restore concern, not an active-active deployment.
{{< /callout >}}

## Embedded Web UI

`buz-manager` embeds `dashboard/dist` at compile time (`rust-embed`).
Production binaries and Docker images need **no** separate `dashboard/`
directory.

## Container deployment

### Images

| Image | Contents | Platforms |
|-------|----------|-----------|
| `buz-manager` | Manager only (dashboard embedded) | linux/amd64 + linux/arm64 |
| `buz-runner` | Runner only | linux/amd64 + linux/arm64 |
| `buz-standalone` | Manager + Runner + entrypoint in one container (lab / single-box) | linux/amd64 + linux/arm64 |

Registries: Aliyun ACR (`<registry>/<namespace>/...`, private) and Docker Hub
(`schiplat/...`, `buz-standalone` public). Tags: `vX.Y.Z`, `X.Y.Z`, `latest`,
plus `-amd64` / `-arm64` platform suffixes.

### Layout

Each image contains **only** the binary + a default config (UI is embedded in
`buz-manager`):

```text
/app/buz/
  buz-manager | buz-runner          # standalone image has both + entrypoint.sh
  conf/manager.toml | conf/runner.toml
  data/  pipelines/          # Manager volumes
  workspace/                 # Runner volume
```

Host templates for Compose live in **`deploy/conf/`**. Compose mounts them
into `/app/buz/conf/...` (`BUZ_MANAGER_CONF` / `BUZ_RUNNER_CONF`).

### Split deploy (Manager + Runner containers)

```bash
export BUZ_JWT_SECRET="$(openssl rand -hex 32)"

# Manager (in the buz repository, `make up` runs the same compose file):
docker compose up -d manager
# UI -> Runners -> New Runner -> paste token into deploy/conf/runner.toml
docker compose up -d runner

# Runner on another machine: point manager_url at the Manager, then
export BUZ_RUNNER_CONF=/etc/buz/runner.toml
docker compose up -d runner
```

Pull images from the registry instead of building:

```bash
docker pull registry.cn-hongkong.aliyuncs.com/<ns>/buz-manager:latest
docker pull registry.cn-hongkong.aliyuncs.com/<ns>/buz-runner:latest
```

### Deploy modes

| Mode | How |
|------|-----|
| Same host | Manager -> UI create Runner -> edit `deploy/conf/runner.toml` -> start Runner |
| Manager only | `docker compose up -d manager` |
| Runner elsewhere | `BUZ_RUNNER_CONF=/etc/buz/runner.toml docker compose up -d runner` |

### One-shot VM deploy (`deploy/dev-deploy.sh`)

For a fresh Linux VM (arm64 or x86_64) that only has Docker:

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
`DOCKER`, `BUZ_CONTAINER_UID`. A `BUZ_JWT_SECRET` is generated on first `up`
and persisted in `$DEPLOY_DIR/.env` so stored secrets stay decryptable across
restarts. `render` writes the compose file + runner config without touching
Docker (useful to preview before deploying).

**Works on both arm64 and amd64 VMs.** The default `IMAGE_TAG=latest` points
to the merged multi-arch manifest. To pin a single platform set a
platform-specific tag instead: `IMAGE_TAG=latest-arm64` (or `latest-amd64` /
`1.7.3-arm64`).

**Runs as a non-root user with sudo.** Every `docker` invocation is prefixed
with `sudo` when the current user cannot reach the daemon directly
(auto-detected; override with `DOCKER='sudo docker'`, or add your user to the
docker group). `$DEPLOY_DIR` is created with sudo when needed and owned by
the caller. The buz containers run as `USER buz` (uid `BUZ_CONTAINER_UID`,
default 10001), so the bind-mounted data dirs are chown'ed to that uid.
`conf/` stays owned by the deploying user so `runner.toml` can be
(re)written by the script.

### GitHub Actions -> personal ACR

Workflows:
- `.github/workflows/docker.yml` - split `buz-manager` / `buz-runner` (amd64 + arm64)
- `.github/workflows/docker-standalone.yml` - combined `buz-standalone` (amd64 + arm64)

Both trigger on `v*` tags or manual `workflow_dispatch`.

Configure the GitHub repository:

| Kind | Name | Example |
|------|------|---------|
| Variable | `ACR_REGISTRY` | `registry.cn-hongkong.aliyuncs.com` |
| Variable | `ACR_NAMESPACE` | your ACR namespace |
| Secret | `ACR_USERNAME` | ACR login user |
| Secret | `ACR_PASSWORD` | ACR password / token |

Pushed tags: `<registry>/<namespace>/buz-manager:v1.7.3` (and `:1.7.3`,
`:latest`), same for `buz-runner` and `buz-standalone` (multi-arch).

### Standalone auth (dev only)

The standalone image enables Manager `[standalone]` so the co-located Runner
may register over loopback without `register_token`, but only if the runner
name is already authorized (`standalone.auto_approve`). Non-loopback
registration is rejected. This does **not** change the production
`register_token` protocol used by split deployments.

**Full first-run and upgrade guide:** [Standalone (single
image)](/docs/03-operations/standalone/). Template Compose:
`deploy/standalone/docker-compose.yml`.

### Runner provisioning requires a platform admin

`runner` auto-creates the runner by calling `buz-manager gen-runner-token`
inside the Manager container. That CLI subcommand verifies the caller holds a
platform-admin credential - either a login JWT or an **API token** (created
in Manager UI -> Access -> API tokens; recommended for automation since it is
revocable and audited via `api_token.*` events) - before touching the
`runners` table. It logs in via `POST /api/v1/auth/login` using
`ADMIN_USERNAME`/`ADMIN_PASSWORD`, or accepts a pre-obtained
`BUZ_ADMIN_TOKEN`. To provision from elsewhere, export the token yourself:

```bash
# Option A: login JWT
ADMIN_TOKEN=$(curl -s -X POST http://127.0.0.1:8080/api/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"<admin>","password":"<password>"}' | jq -r .data.token)
# Non-root user: use `sudo docker` (or whatever DOCKER resolves to)
docker exec buz-manager /app/buz/buz-manager -c /app/buz/conf/manager.toml \
  gen-runner-token linux-vm-runner --labels linux-x64 --token "$ADMIN_TOKEN"
# name: linux-vm-runner
# register_token: <one-shot token>   -> paste into conf/runner.toml

# Option B (preferred for scripts): API token from Access -> API tokens
BUZ_ADMIN_TOKEN='buz_...' docker exec buz-manager \
  /app/buz/buz-manager -c /app/buz/conf/manager.toml \
  gen-runner-token linux-vm-runner --labels linux-x64
```

### Troubleshooting: Runner stuck in a restart loop with `Cannot reach Buz Manager`

If `docker logs buz-runner` shows `error sending request for url
(http://manager:8080/...)` while `docker ps` shows `buz-manager` is
`Up (healthy)`, usual causes are: (1) Runner started before Manager was
ready, or (2) compose DNS alias `manager` went stale after Manager was
recreated.

One-shot fix on a `dev-deploy.sh` host:

```bash
# Prefer upgrade (waits for Manager health, recreates Runner after sleep 3s).
sudo SKIP_PULL=1 IMAGE_TAG=<current-or-new-tag> ./dev-deploy.sh upgrade
# Or just repair Runner:
sudo SKIP_PULL=1 ./dev-deploy.sh runner
```

Mixing plain `docker` (docker group / rootless) with `sudo docker` (rootful)
talks to different daemons - Runner recreated on the wrong side never reaches
Manager. `dev-deploy.sh` prefers the daemon that already has `buz-manager`.

Manual check: `sudo docker exec buz-runner getent hosts manager` (should
return the Manager container IP) and compare against
`sudo docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' buz-manager`.
Same-compose default is container-to-container: `manager_url =
"http://manager:8080"` with `depends_on: manager: condition: service_healthy`.

**Upgrading Runner and Manager authentication behavior.** Where practical,
upgrade Runners before the Manager, then upgrade the Manager. New Runner
builds persist their Manager-issued JWT at `runner.token_cache_path` (default
`./state/runner-token`) outside the job workspace and present it on restart,
so subsequent registrations refresh without the one-shot register token.
Persist this state path across restarts; on Unix the credential is stored
with mode `0600`. The newer Manager intentionally provides no compatibility
grace period that accepts an old Runner based only on its name.

For upgrades, if the new cache path is missing the Runner reads the legacy
`<workspace_dir>/runner-token` file and migrates it after a successful
registration. If the new cache exists but cannot be read or is insecure,
startup fails rather than silently using an older credential. A cache write
failure after registration also aborts before jobs are accepted. If no valid
cached JWT remains, reissue its register token from Dashboard -> **Runners**
-> key icon. Copy the token shown once into the Runner's effective
`register_token` setting (TOML or `BUZ_REGISTER_TOKEN`) and restart the
Runner. Platform admins can also call
`POST /api/v1/runners/{id}/register-token/reissue`; see the [API
reference](/docs/01-architecture/platform/api-reference/). This remains a
manual, out-of-band recovery flow; the Manager cannot update or restart a
disconnected Runner.

## Binary deployment

Deploy the release archives from
[GitHub Releases](https://github.com/schiplat/buz/releases) directly - no
compilation. Each archive contains the binaries, default configs, and
sample pipelines:

```text
buz-<version>-linux-x86_64.tar.gz
  bin/buz-manager   # dashboard embedded
  bin/buz-runner
  conf/manager.toml
  conf/runner.toml
  data/  pipelines/
  README.md
```

### Manager

```bash
# Download and extract (linux-x86_64 example; linux-aarch64 for arm64 hosts)
curl -fsSLO https://github.com/schiplat/buz/releases/download/v<version>/buz-<version>-linux-x86_64.tar.gz
tar -xzf buz-<version>-linux-x86_64.tar.gz && cd buz-<version>-linux-x86_64

# Create config (or reuse the shipped conf/manager.toml)
mkdir -p conf data
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
# Import/export directory only; definitions live in the database.
yaml_scan_dir = "./pipelines"
yaml_scan_interval_seconds = 30
EOF

# Start (run under systemd or a process supervisor in production)
export BUZ_JWT_SECRET="$(openssl rand -hex 32)"
./bin/buz-manager -c conf/manager.toml
# Open https://... or http://... -> /setup to create the first admin
```

For HTTPS (Manager native TLS, reverse proxy, full commands), see
**[HTTPS Communication](/docs/03-operations/tls/)**.

### Runner

Runners can be deployed on any machine with outbound HTTPS access to the
Manager.

> **Trust boundary.** A Runner executes every job as its own OS user - jobs
> are not sandboxed from one another or from the Runner's credentials. A
> **Shared** Runner (no team binding) runs jobs from every team: use it only
> when all those teams trust each other. Put mutually untrusted teams on
> dedicated Runner hosts/OS users, and treat Docker socket access as
> host-root-equivalent. See [RBAC - Tenancy
> Model](/docs/01-architecture/platform/rbac/).

```bash
mkdir -p conf
cat > conf/runner.toml <<EOF
[runner]
name = "prod-runner-01"
manager_url = "https://buz-api.example.com"
register_token = "your-register-token"
workspace_dir = "/tmp/buz-runner"
token_cache_path = "./state/runner-token" # persist this path across restarts

[engine]
# ssh_known_hosts_path = "/home/runner/.ssh/known_hosts"

[logging]
level = "info"
EOF

export BUZ_REGISTER_TOKEN="your-token"
./bin/buz-runner
```

> Enable Docker steps (`engine: docker`) in **Manager UI -> Runners ->
> Docker**. See [Docker Engine](/docs/01-architecture/execution/docker-engine/).
> Host-side `docker build` / SSH `docker ...` does **not** need this flag.

## Requirements

| Component | Minimum |
|-----------|---------|
| Runner memory (idle) | < 20MB |
| Manager memory (idle) | < 50MB |
| Network | Runner -> Manager outbound HTTPS only |
| Database | SQLite (built-in, zero setup) |

## Reverse Proxy (Nginx, HTTP-only)

`buz-manager` serves both the API and the embedded Web UI on a single port.
Put Nginx in front so only the proxy is exposed publicly.

Templates:
- `deploy/nginx/buz-manager.conf` - manager running directly on the host (HTTP)
- `deploy/nginx/buz-manager-docker.conf` - manager started via `deploy/dev-deploy.sh` (Docker, HTTP)
- `deploy/nginx/buz-manager-ssl.conf` - HTTPS + redirect

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

- WebSocket upgrade for live job logs (`/api/v1/jobs/{id}/logs/stream`) and
  pipeline stream
- Runner long-poll (50s) + WS keepalive timeouts
- `client_max_body_size` for pipeline YAML import and log uploads
- gzip for embedded UI assets and JSON

Runners behind the proxy:

```toml
[runner]
manager_url = "http://buz.example.com"   # no :8080; proxy forwards to Manager
```

For HTTPS, add `listen 443 ssl` + certificate lines to the site config (see
[HTTPS Communication](/docs/03-operations/tls/)).

## Troubleshooting: sidebar clicks dead after Manager restart

**Symptom:** After stopping and restarting Manager, sidebar links often do
nothing on click, but the same `href` still works via address bar / new tab.

**Cause:** While Manager is down, in-flight API polls / hung requests occupy
the browser per-host connection pool. SPA route lazy-imports share that pool,
so `router.push` hangs. Full page loads use a fresh context and still work.

**Mitigations in the UI (already shipped):**

- Sidebar uses `safeNavigate` - if SPA navigation does not settle within
  ~4s, it hard-loads the target URL.
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
| `navigation timed out - hard fallback` | Connection pool / chunk load hung; full reload used |
| `api request` / `inFlight` | How many HTTP calls are outstanding |
| `router.onError` | Chunk / dynamic import failure |

Disable with `localStorage.removeItem('buz_debug_nav')`.

### SSH host keys (production)

New runners default to `known_hosts` (secure since v1.6): server keys are
verified against the runner's known_hosts file, defaulting to
`~/.ssh/known_hosts` - add your hosts once via `ssh-keyscan` or a first
manual `ssh`. Runners created before v1.6 keep their stored `insecure`
value; flip them in Manager UI -> Runners if you want verification. To point
at a specific file, set it in `runner.toml`:

```toml
[engine]
ssh_known_hosts_path = "/home/runner/.ssh/known_hosts"  # optional; default ~/.ssh/known_hosts
```

Pre-seed keys with `ssh-keyscan -p 22 host >> ~/.ssh/known_hosts`. A changed
host key fails the step with an explicit error.
