---
title: Standalone (single image)
weight: 3
description: "First run and upgrade for buz-standalone (Manager + Runner in one container, arm64)"
---

# Standalone image — first run & upgrade

`buz-standalone` packs **Manager + Runner** in one container. It is intended for
**dev / lab / single-VM demos** (arm64 only). Production split deploy continues
to use `buz-manager` + `buz-runner` with normal `register_token` registration.

| Item | Value |
|------|--------|
| Image | `registry.cn-hongkong.aliyuncs.com/example/buz-standalone` |
| Arch | **linux/arm64** only |
| Tags | `latest`, `vX.Y.Z`, `X.Y.Z`, plus `-arm64` suffixes |
| Auth mode | Manager `[standalone]` on — loopback tokenless register for `standalone-runner` |
| Compose template | [`deploy/standalone/docker-compose.yml`](https://github.com/hzbd/buz/blob/master/deploy/standalone/docker-compose.yml) |

---

## How it works

```text
┌─ container (buz) ─────────────────────────────────┐
│  entrypoint.sh (BUZ_MODE=all)                     │
│    ├─ buz-manager  :8080                          │
│    └─ buz-runner  → http://127.0.0.1:8080         │
│         (no register_token; name=standalone-runner)│
└───────────────────────────────────────────────────┘
         ▲
         │ host :8080 → container :8080
```

1. Manager boots with `[standalone] enabled = true` and
   `auto_approve = ["standalone-runner"]` (creates that runner row if missing).
2. Entrypoint waits until Manager listens on `:8080`, then starts Runner.
3. Runner registers over **loopback** without a token. Non-loopback register is
   rejected (external machines cannot join this Manager as runners).

**Do not** use this image if you need remote runners or multi-host production.

---

## Prerequisites

- Linux **arm64** host (Apple Silicon VM, Graviton, arm64 cloud VM, …)
- Docker (or `sudo docker`) with compose v2 recommended
- Outbound access to ACR (or a pre-pulled image)
- ACR login if the registry is private:

```bash
docker login registry.cn-hongkong.aliyuncs.com
# or: sudo docker login …
```

---

## First run (recommended: Compose)

### 1. Create a deploy directory

```bash
sudo mkdir -p /opt/buz-standalone
sudo chown "$(id -u):$(id -g)" /opt/buz-standalone
cd /opt/buz-standalone

mkdir -p data pipelines workspace logs
# Container runs as uid 10001 (USER buz)
sudo chown -R 10001:10001 data pipelines workspace logs
```

### 2. Persist the JWT secret

Losing `BUZ_JWT_SECRET` makes previously stored Secrets / Host credentials
undecryptable. Generate once and keep it next to Compose:

```bash
# .env is read automatically by docker compose
cat > .env <<EOF
BUZ_JWT_SECRET=$(openssl rand -hex 32)
IMAGE_TAG=latest
EOF
chmod 600 .env
```

### 3. Write Compose file

Copy from the repo, or create `docker-compose.yml`:

```yaml
# /opt/buz-standalone/docker-compose.yml
services:
  buz:
    image: registry.cn-hongkong.aliyuncs.com/example/buz-standalone:${IMAGE_TAG:-latest}
    container_name: buz
    restart: unless-stopped
    ports:
      - "${BUZ_PORT:-8080}:8080"
    environment:
      BUZ_JWT_SECRET: ${BUZ_JWT_SECRET}
      # Optional: BUZ_MODE=all|manager|runner  (default all)
    volumes:
      - ./data:/app/buz/data
      - ./pipelines:/app/buz/pipelines
      - ./workspace:/app/buz/workspace
      - ./logs:/app/buz/logs
```

A checked-in copy lives at `deploy/standalone/docker-compose.yml`.

### 4. Pull and start

```bash
# If your user cannot talk to the daemon:
#   export DOCKER='sudo docker'
#   alias docker='sudo docker'   # or prefix each command

docker compose pull
docker compose up -d
docker compose ps
docker compose logs -f --tail=100
```

Healthy signs in logs:

- `STANDALONE DEV MODE enabled …`
- `standalone auto_approve: created authorized runner` (first boot) or `already exists`
- `standalone: Manager is accepting connections on :8080`
- `registered with Manager` / `runner registered` (Runner)

### 5. Bootstrap the admin UI

1. Open `http://<host>:8080` → redirected to **`/setup`**
2. Create the first platform admin
3. Sign in → **Runners** — `standalone-runner` should appear **online**
   (if still offline, wait a few seconds or `docker compose restart`)

You do **not** need Runners → New Runner or a `register_token` for the
built-in co-located runner.

### 6. Smoke check

```bash
curl -fsS "http://127.0.0.1:8080/health"
docker compose logs --tail=50 | grep -E 'registered|STANDALONE|ERROR' || true
```

---

## First run (plain `docker run`)

```bash
export BUZ_JWT_SECRET="$(openssl rand -hex 32)"
# Save this value — required on every restart/upgrade
echo "BUZ_JWT_SECRET=${BUZ_JWT_SECRET}" >> /opt/buz-standalone/.env

docker pull registry.cn-hongkong.aliyuncs.com/example/buz-standalone:latest

docker run -d --name buz --restart unless-stopped \
  -p 8080:8080 \
  -e BUZ_JWT_SECRET \
  -v /opt/buz-standalone/data:/app/buz/data \
  -v /opt/buz-standalone/pipelines:/app/buz/pipelines \
  -v /opt/buz-standalone/workspace:/app/buz/workspace \
  -v /opt/buz-standalone/logs:/app/buz/logs \
  registry.cn-hongkong.aliyuncs.com/example/buz-standalone:latest
```

Then complete `/setup` as above.

---

## Upgrade

Goal: new image, **same data volumes**, **same `BUZ_JWT_SECRET`**.

### Compose (preferred)

```bash
cd /opt/buz-standalone

# Pin a release tag when you want a fixed version (recommended):
#   echo 'IMAGE_TAG=v1.3.5' >> .env
#   # or: IMAGE_TAG=v1.4.0 docker compose up -d

docker compose pull
docker compose up -d

docker compose ps
docker compose logs --tail=80
curl -fsS "http://127.0.0.1:8080/health"
```

Compose recreates the container when the image digest changes; named/bind
volumes under `./data` etc. are kept.

### Plain Docker

```bash
cd /opt/buz-standalone
set -a && source .env && set +a   # reload BUZ_JWT_SECRET

docker pull registry.cn-hongkong.aliyuncs.com/example/buz-standalone:${IMAGE_TAG:-latest}

docker stop buz
docker rm buz

docker run -d --name buz --restart unless-stopped \
  -p 8080:8080 \
  -e BUZ_JWT_SECRET \
  -v /opt/buz-standalone/data:/app/buz/data \
  -v /opt/buz-standalone/pipelines:/app/buz/pipelines \
  -v /opt/buz-standalone/workspace:/app/buz/workspace \
  -v /opt/buz-standalone/logs:/app/buz/logs \
  registry.cn-hongkong.aliyuncs.com/example/buz-standalone:${IMAGE_TAG:-latest}
```

### After upgrade checklist

| Check | Command / UI |
|-------|----------------|
| Health | `curl -fsS http://127.0.0.1:8080/health` |
| Runner online | UI → Runners → `standalone-runner` |
| Secrets still decrypt | Open an existing Secret / Host — if JWT secret changed, re-enter values |
| Job history | UI → Jobs (stored under `./data`) |

### Rollback

```bash
# Compose: set IMAGE_TAG to the previous tag, then:
docker compose pull
docker compose up -d

# Or docker run with an explicit older tag, same volumes + same BUZ_JWT_SECRET
```

---

## Configuration reference

### Environment

| Variable | Required | Meaning |
|----------|----------|---------|
| `BUZ_JWT_SECRET` | **Yes** | Encrypts stored secrets; must stay stable across upgrades |
| `BUZ_MODE` | No | `all` (default) / `manager` / `runner` |
| `BUZ_PORT` | No | Host port in Compose example (default `8080`) |
| `IMAGE_TAG` | No | Image tag for Compose (default `latest`) |
| `BUZ_ALLOW_INSECURE_JWT` | No | Allow weak JWT for labs only — never in shared environments |

### Volumes

| Host path (example) | Container | Contents |
|---------------------|-----------|----------|
| `./data` | `/app/buz/data` | SQLite DB, encrypted secrets |
| `./pipelines` | `/app/buz/pipelines` | YAML scan dir |
| `./workspace` | `/app/buz/workspace` | Runner job workspaces |
| `./logs` | `/app/buz/logs` | Runner file logs (also check `docker logs`) |

Bind mounts must be writable by **uid 10001** (`chown -R 10001:10001 …`).

### Image defaults (inside the image)

- Manager: `deploy/conf/manager.standalone.toml` → `/app/buz/conf/manager.toml`
  - `[standalone] enabled = true`
  - `auto_approve = ["standalone-runner"]`
- Runner: `deploy/conf/runner.standalone.toml` → `/app/buz/conf/runner.toml`
  - `name = "standalone-runner"`
  - `manager_url = "http://127.0.0.1:8080"`
  - `standalone = true`
  - `register_token = ""` (tokenless loopback register)

Config key reference: [Configuration — Standalone mode](/docs/03-operations/configuration/#standalone-mode).

To override configs, bind-mount your own files over those paths (advanced).

---

## Troubleshooting

**Runner restart loop / not online**

```bash
docker compose logs --tail=100
# Expect loopback register; should NOT see "Cannot reach Buz Manager"
# Should NOT see "register_token is required"
```

Restart once after Manager finished migrations:

```bash
docker compose restart
```

**`refusing to start: auth.jwt_secret is missing`**

`BUZ_JWT_SECRET` was not passed into the container. Fix `.env` / `-e` and recreate.

**Secrets garbage after upgrade**

`BUZ_JWT_SECRET` changed. Restore the old secret from backup `.env`, or re-enter
Secrets/Hosts under the new secret.

**Pull TLS timeout to ACR**

```bash
# Retry later, or use a machine that already has the image:
docker images | grep buz-standalone
# Compose still needs the tag present locally if pull fails
```

**Need a second / remote Runner**

Standalone Manager **rejects non-loopback** registration. Use the normal
two-image deploy (`buz-manager` + `buz-runner`) instead — see
[Deployment](/docs/03-operations/deployment/).

**Host sleep → runner shows offline**

When the physical host (or hypervisor) suspends, the whole VM — and the
standalone container inside it — freezes. The runner stops heartbeating, so the
Manager marks it `offline` after `heartbeat_timeout_seconds` (default 90s).

Recovery (manual, no code change needed):

1. Wake the host. The container's processes are still alive (suspend ≠ kill),
   so the runner resumes heartbeating and flips back to `online` on its own
   within ~30s. **Wait ~60s first — no action needed.**
2. If it stays offline after ~60s, the container was stopped (the host was
   powered off/rebooted, not suspended). Bring it back manually:

```bash
ssh <vm>
cd /opt/buz-standalone        # your deploy dir
docker compose ps             # confirm state: running vs exited
docker compose up -d          # or: docker compose restart buz
docker compose logs --tail=50 | grep -E 'registered|ERROR'
```

The Runners page should show `online` again within a few seconds.

**Wrong architecture**

Image is arm64-only. On amd64 hosts Docker will fail to run (or need emulation,
which is unsupported here). Use split amd64 images or an arm64 VM.

---

## Security notes (dev-only)

- `[standalone]` is for **same-container / loopback** pairing only.
- Publishing `:8080` on a public IP exposes the Manager UI — protect with VPN,
  firewall, or reverse-proxy auth.
- Do not enable Manager standalone mode on a production multi-runner control plane.
