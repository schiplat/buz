# Deploy templates (Docker + tar.gz release)

| Path | Used by |
|------|---------|
| `conf/manager.toml` | Docker / Compose / release tarball |
| `conf/runner.toml` | Same |
| `dev-deploy.sh` | One-shot deploy of Manager + Runner on a Linux VM (pull from ACR + compose) |
| `nginx/buz-manager.conf` | Optional system Nginx reverse proxy (HTTP-only) |
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
./dev-deploy.sh login   # docker login to ACR (or set ACR_USERNAME / ACR_PASSWORD)
./dev-deploy.sh up      # pull buz-manager/buz-runner from ACR + start Manager
# First login in the web UI (http://<vm>:8080) to create the platform admin,
# then bootstrap the Runner on the same VM:
ADMIN_USERNAME='<admin>' ADMIN_PASSWORD='<password>' ./dev-deploy.sh runner
./dev-deploy.sh status | logs | down
```

Runs on a Linux VM with only Docker installed; images are pulled from
`${ACR_REGISTRY}/${ACR_NAMESPACE}/buz-{manager,runner}:${IMAGE_TAG}` (multi-arch,
default `registry.cn-hongkong.aliyuncs.com/example`, tag `latest`). Compose +
configs land in `$DEPLOY_DIR` (default `/opt/buz`); `BUZ_JWT_SECRET` is generated
on first `up` and persisted there. `render` previews generated files without
touching Docker.

`./dev-deploy.sh runner` calls `buz-manager gen-runner-token` **inside the Manager
container** to create the runner and capture its register_token, so no web UI
Runners page is required. Provisioning a runner is restricted to platform
admins: the script logs in via `POST /api/v1/auth/login` with
`ADMIN_USERNAME`/`ADMIN_PASSWORD`, or accepts a pre-obtained `BUZ_ADMIN_TOKEN`
(either a login JWT or an **API token** created in Manager UI → Access → API
tokens) and passes it to `gen-runner-token`. API tokens are the recommended
automation credential: they can be revoked independently and are recorded in
the audit log (`api_token.*`). Set `RUNNER_TOKEN` explicitly to override this
behaviour (e.g. when the runner lives on a different host than Manager).

Logs default to **container stdout** (`docker logs`). Set `logging.file` only if you need a file inside the volume.

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
