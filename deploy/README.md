# Deploy templates (Docker + tar.gz release)

| Path | Used by |
|------|---------|
| `conf/manager.toml` | Docker / Compose / release tarball |
| `conf/runner.toml` | Same |

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
docker compose up -d runner
```

Logs default to **container stdout** (`docker logs`). Set `logging.file` only if you need a file inside the volume.

## Binary tarball

```bash
make package   # builds for the current host (macOS or Linux)
# CI Release workflow also publishes linux-x86_64/aarch64 + macos-x86_64/aarch64
```
Packaged `conf/manager.toml` ships with an **empty** `jwt_secret`. Before start:

```bash
export BUZ_JWT_SECRET="$(openssl rand -hex 32)"
./bin/buz-manager -c conf/manager.toml
```

Manager **refuses to start** on empty or known template secrets unless `BUZ_ALLOW_INSECURE_JWT=1` (never for production).
