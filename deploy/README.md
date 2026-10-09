# Deploy templates (Docker + tar.gz release)

| Path | Used by |
|------|---------|
| `conf/manager.toml` | Docker / Compose / release tarball |
| `conf/manager.standalone.toml` | Standalone image Manager conf (`[standalone]` on) |
| `conf/runner.toml` | Same |
| `conf/runner.standalone.toml` | Standalone image Runner conf (loopback, no token) |
| `standalone/entrypoint.sh` | Starts Manager + Runner in one container |
| `dev-deploy.sh` | One-shot deploy of Manager + Runner on a Linux VM (pull from a container registry + compose) |
| `nginx/buz-manager.conf` | Optional system Nginx reverse proxy (HTTP-only, non-Docker) |
| `nginx/buz-manager-docker.conf` | Same, for `dev-deploy.sh` Docker deployments (HTTP-only) |
| `nginx/buz-manager-ssl.conf` | Same, HTTPS (Let's Encrypt + HTTP→HTTPS redirect) |

## Container layout

```text
/app/buz/buz-manager | buz-runner
/app/buz/conf/*.toml
/app/buz/data | pipelines | workspace
```

## Runner.toml shape

```toml
[runner]
name = "..."
manager_url = "http://manager:8080"
register_token = "..."
workspace_dir = "./workspace"

[engine]
# ssh_known_hosts_path = "..."

[logging]
level = "info"
# file = "./logs/runner.log"
```

## Docker (same host)

```bash
export BUZ_JWT_SECRET="$(openssl rand -hex 32)"
docker compose up -d manager
# UI → Runners → New Runner → edit conf/runner.toml
# or, without the UI:  ./bin/buz-manager -c conf/manager.toml gen-runner-token <name> --token <admin-jwt>
docker compose up -d runner
```

### VM one-shot deploy (`dev-deploy.sh`)

```bash
./dev-deploy.sh login   # docker login (or set REGISTRY_USERNAME / REGISTRY_PASSWORD)
./dev-deploy.sh up      # pull buz-manager/buz-runner from Docker Hub + start Manager
# First login in the web UI (http://<vm>:8080) to create the platform admin,
# then bootstrap the Runner on the same VM:
ADMIN_USERNAME='<admin>' ADMIN_PASSWORD='<password>' ./dev-deploy.sh runner
./dev-deploy.sh status | logs | down
```

**Upgrading an existing deployment** (Manager + Runner at once) — pick the new tag
explicitly rather than `latest`, so the running version stays traceable:

```bash
IMAGE_TAG=v1.7.3 ./dev-deploy.sh upgrade
```

`upgrade` rewrites the `image:` lines in the compose file (keeping any other
edits you made), pulls the new images, **force-recreates Manager and waits for
`/health`**, sleeps 3s, then force-recreates Runner and waits until it registers
(API online check when admin credentials are set, otherwise runner logs).
Runner talks to Manager on the compose network (`manager_url =
http://manager:8080`); if a prior `host.docker.internal` migration is still on
disk, upgrade/runner strips `extra_hosts` and rewrites `manager_url` back.
`BUZ_JWT_SECRET` (in `$DEPLOY_DIR/.env`) and the bind-mounted `data/`,
`workspace/`, `logs/` dirs are untouched. If verification still fails, re-run
`./dev-deploy.sh runner` with admin credentials.

`docker pull` always contacts the registry even when the tag already exists locally
(manifest refresh). On TLS/network timeouts, the script falls back to the local
image when present. To skip the registry entirely:

```bash
SKIP_PULL=1 IMAGE_TAG=v1.7.3 ./dev-deploy.sh upgrade
# If the stack was first created with sudo/root (common on VMs):
sudo SKIP_PULL=1 IMAGE_TAG=v1.7.3 ./dev-deploy.sh upgrade
# or: DOCKER='sudo docker' SKIP_PULL=1 IMAGE_TAG=v1.7.3 ./dev-deploy.sh upgrade
```

Runs on a Linux VM with only Docker installed; images are pulled from
`${REGISTRY}/${NAMESPACE}/buz-{manager,runner}:${IMAGE_TAG}` (multi-arch,
default `docker.io/schiplat`, tag `latest`; set REGISTRY/NAMESPACE to use a mirror or your own push target). Compose +
configs land in `$DEPLOY_DIR` (default `/opt/buz`); `BUZ_JWT_SECRET` is generated
on first `up` and persisted there. `render` previews generated files without
touching Docker.

The default `latest` is the merged multi-arch manifest, so Docker auto-selects
the image for the VM's architecture (arm64 or amd64). To pin one platform —
e.g. while the CI merge job is still running, or to bypass a broken index —
set `IMAGE_TAG=latest-arm64` / `latest-amd64` (or `1.7.3-arm64`). Verify a
platform tag with `docker manifest inspect
<registry>/<namespace>/buz-manager:1.7.3-arm64`.

**Non-root user with sudo is supported.** The script auto-detects whether the
current user can reach the docker daemon directly; if not it falls back to
`sudo docker` for every invocation (override with `DOCKER='sudo docker'`, or add
your user to the docker group). `$DEPLOY_DIR` is created with sudo when needed
and owned by the caller. Because the buz containers run as `USER buz`
(uid `BUZ_CONTAINER_UID`, default 10001), the bind-mounted data dirs
(`data`, `pipelines`, `workspace`, `logs`) are chown'ed to that uid so the
containers can write even though the host user is not root. `conf/` stays with
the deploying user (the script writes `runner.toml` there).

`./dev-deploy.sh runner` calls `buz-manager gen-runner-token` **inside the Manager
container** to create the runner and capture its register_token, so no web UI
Runners page is required. After starting, it **waits for the runner to report
online** (polling the Manager runners API) and force-recreates the container on
each retry — repairing the intermittent stale-DNS restart loop without manual
intervention. Provisioning a runner is restricted to platform
admins: the script logs in via `POST /api/v1/auth/login` with
`ADMIN_USERNAME`/`ADMIN_PASSWORD`, or accepts a pre-obtained `BUZ_ADMIN_TOKEN`
(either a login JWT or an **API token** created in Manager UI → Access → API
tokens) and passes it to `gen-runner-token`. API tokens are the recommended
automation credential: they can be revoked independently and are recorded in
the audit log (`api_token.*`). Set `RUNNER_TOKEN` explicitly to override this
behaviour (e.g. when the runner lives on a different host than Manager).

Logs default to **container stdout** (`docker logs`). Set `logging.file` only if you need a file inside the volume.

## Standalone image (Manager + Runner, arm64)

**Full guide (first run + upgrade):**  
[`docs/content/docs/03-operations/standalone.md`](../docs/content/docs/03-operations/standalone.md)  
Compose template: [`deploy/standalone/docker-compose.yml`](standalone/docker-compose.yml)

Quick start:

```bash
mkdir -p /opt/buz-standalone && cd /opt/buz-standalone
# copy deploy/standalone/docker-compose.yml here
mkdir -p data pipelines workspace logs && sudo chown -R 10001:10001 data pipelines workspace logs
cat > .env <<EOF
BUZ_JWT_SECRET=$(openssl rand -hex 32)
IMAGE_TAG=latest
EOF
chmod 600 .env
docker compose pull && docker compose up -d
# http://<host>:8080 → /setup → Runners → standalone-runner online
```

Upgrade (same `.env` + volumes):

```bash
cd /opt/buz-standalone
docker compose pull && docker compose up -d
```

Dev-only: Manager `[standalone]` + loopback tokenless register for
`standalone-runner`. Split Manager/Runner deploys keep normal `register_token`.

## Binary tarball

```bash
make package   # builds for the current host (macOS or Linux)
# CI Release workflow publishes linux-x86_64/aarch64 + macos-aarch64
# (macos-x86_64 / Intel temporarily disabled)
```
Packaged `conf/manager.toml` ships with an **empty** `jwt_secret`. Before start:

```bash
export BUZ_JWT_SECRET="$(openssl rand -hex 32)"
./bin/buz-manager -c conf/manager.toml
```

Manager **refuses to start** on empty or known template secrets unless `BUZ_ALLOW_INSECURE_JWT=1` (never for production).
