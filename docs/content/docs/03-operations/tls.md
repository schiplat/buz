---
title: HTTPS Communication
weight: 4
description: "Step-by-step HTTPS for Manager ↔ Runner (and UI) — commands, configs, Docker, reverse proxy"
---

Buz HTTPS is **runtime configuration** — not a build-time switch. The release binary / Docker image already includes TLS support.

| Component | Role |
|-----------|------|
| **Manager** | HTTPS **server** (API + embedded Web UI on the same port) |
| **Runner** | HTTPS **client** only (outbound to Manager; no TLS listener) |

Two common patterns:

1. **Manager terminates TLS** — PEM cert/key in `manager.toml`
2. **Reverse proxy terminates TLS** — Nginx/Caddy in front; Manager stays HTTP on localhost

---

## Prerequisites

```bash
openssl version   # need OpenSSL 1.1+ (SAN via -addext needs OpenSSL 3 preferred)
```

Decide how clients reach Manager:

| Clients use | Certificate must cover |
|-------------|------------------------|
| `https://buz.example.com` | DNS `buz.example.com` |
| `https://10.0.0.5:8080` | IP `10.0.0.5` |
| `https://localhost:8080` | DNS `localhost` + preferably `127.0.0.1` |

---

## Pattern A — Manager native TLS

### A1. Create directories (binary package or host)

```bash
# From the directory you will run Manager in (WORKDIR), e.g. /app/buz or extract dir
mkdir -p certs data pipelines conf logs
```

### A2. Generate a self-signed certificate (lab / private network)

Replace hostnames/IPs with yours:

```bash
export BUZ_HOST="buz.example.com"
export BUZ_IP="10.0.0.5"

openssl req -x509 -newkey rsa:4096 -nodes \
  -keyout certs/key.pem \
  -out certs/cert.pem \
  -days 825 \
  -subj "/CN=${BUZ_HOST}" \
  -addext "subjectAltName=DNS:${BUZ_HOST},DNS:localhost,IP:127.0.0.1,IP:${BUZ_IP}"

chmod 600 certs/key.pem
chmod 644 certs/cert.pem
ls -l certs/
openssl x509 -in certs/cert.pem -noout -subject -dates -ext subjectAltName
```

**Public CA / Let's Encrypt:** put the issued full-chain PEM and private key at `certs/cert.pem` and `certs/key.pem` (or any paths you prefer), then skip self-signed generation.

### A3. Manager config

Edit `conf/manager.toml` (or `deploy/conf/manager.toml` when using Compose mounts):

```toml
[server]
host = "0.0.0.0"
port = 8080
tls_cert = "certs/cert.pem"
tls_key  = "certs/key.pem"

[database]
path = "./data/buz.db"

[auth]
# Prefer: export BUZ_JWT_SECRET="$(openssl rand -hex 32)"
jwt_secret = ""

[logging]
level = "info"
```

Or set secret via env:

```bash
export BUZ_JWT_SECRET="$(openssl rand -hex 32)"
```

### A4. Start Manager (binary)

```bash
# CWD must be able to resolve tls_cert / tls_key / database paths
./bin/buz-manager -c conf/manager.toml
# Log line should indicate HTTPS / Web UI https ready
```

### A5. Verify Manager HTTPS

```bash
# Self-signed: -k skips verify for this smoke test only
curl -k -sS -o /dev/null -w "%{http_code}\n" https://127.0.0.1:8080/
curl -k -sS https://127.0.0.1:8080/api/v1/health

# With a trusted cert (no -k):
# curl -sS https://buz.example.com:8080/api/v1/health
```

Browser: open `https://<host>:8080` → `/setup` or `/login`. Expect a warning for self-signed certs until the OS/browser trusts the CA/cert.

### A6. Runner config (HTTPS client)

```toml
[runner]
name = "prod-runner-01"
manager_url = "https://buz.example.com:8080"   # must be https://
register_token = "PASTE_FROM_UI"
workspace_dir = "./workspace"
# Lab / self-signed only:
tls_skip_verify = true

[logging]
level = "info"
file = "./logs/runner.log"
```

**Production:** set `tls_skip_verify = false` and trust the Manager certificate on the Runner host (see [Trust the certificate](#trust-the-certificate-production)).

```bash
./bin/buz-runner -c conf/runner.toml
# Expect: registered with Manager …
```

### A7. Docker Compose (Manager TLS)

1. Put PEMs next to compose or under a host path, e.g. `./certs/`.
2. Mount them into the container and point config at container paths.

Example overlay mounts (add to your compose / override):

```yaml
services:
  manager:
    environment:
      BUZ_JWT_SECRET: ${BUZ_JWT_SECRET}
    volumes:
      - ./deploy/conf/manager.toml:/app/buz/conf/manager.toml:ro
      - ./certs:/app/buz/certs:ro
      - buz-data:/app/buz/data
```

In the **mounted** `manager.toml`:

```toml
[server]
tls_cert = "certs/cert.pem"
tls_key  = "certs/key.pem"
```

```bash
export BUZ_JWT_SECRET="$(openssl rand -hex 32)"
docker compose up -d manager
docker logs -f "$(docker compose ps -q manager)"
curl -k -sS https://127.0.0.1:8080/api/v1/health
```

Runner on the same compose network:

```toml
[runner]
manager_url = "https://manager:8080"
tls_skip_verify = true   # until you mount a CA into the runner image/OS
register_token = "…"
name = "docker-runner-01"
workspace_dir = "./workspace"
```

---

## Pattern B — Reverse proxy terminates TLS

Manager listens HTTP on localhost; proxy presents HTTPS to the world. Good with Let's Encrypt.

### B1. Manager stays HTTP

```toml
[server]
host = "127.0.0.1"
port = 8080
# tls_cert / tls_key unset
```

```bash
./bin/buz-manager -c conf/manager.toml
curl -sS http://127.0.0.1:8080/api/v1/health
```

### B2. Caddy example

```bash
# /etc/caddy/Caddyfile
buz.example.com {
  reverse_proxy 127.0.0.1:8080
}
```

```bash
sudo systemctl reload caddy
curl -sS https://buz.example.com/api/v1/health
```

### B3. Nginx example

A complete HTTPS site config is provided at **`deploy/nginx/buz-manager-ssl.conf`** — copy it, replace `server_name` (and cert paths), reload, and you are live.

```bash
# 1) Manager already bound to loopback (B1 above)
# 2) Issue certs once
sudo certbot certonly --nginx -d buz.example.com
# 3) Install the site
sudo cp deploy/nginx/buz-manager-ssl.conf /etc/nginx/sites-available/buz-manager-ssl.conf
sudo ln -s ../sites-available/buz-manager-ssl.conf /etc/nginx/sites-enabled/buz-manager-ssl.conf
# 4) Edit server_name + cert paths, then test & reload
sudo nginx -t && sudo systemctl reload nginx
```

Equivalent inline config:

```nginx
# HTTP → HTTPS redirect
server {
  listen 80;
  server_name buz.example.com;
  return 301 https://$host$request_uri;
}

server {
  listen 443 ssl http2;
  server_name buz.example.com;

  ssl_certificate     /etc/letsencrypt/live/buz.example.com/fullchain.pem;
  ssl_certificate_key /etc/letsencrypt/live/buz.example.com/privkey.pem;
  ssl_protocols       TLSv1.2 TLSv1.3;

  client_max_body_size 20m;

  gzip on;
  gzip_min_length 1024;
  gzip_types text/plain text/css application/javascript application/json
             image/svg+xml application/xml;

  location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    # WebSocket (job logs) — keep Upgrade headers
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    # Runner long-poll is up to 50s; WS idle keepalive must survive.
    proxy_connect_timeout 60s;
    proxy_send_timeout 3600s;
    proxy_read_timeout 3600s;
  }
}
```

```bash
curl -sS https://buz.example.com/api/v1/health
```

### B4. Runner behind reverse proxy

```toml
[runner]
manager_url = "https://buz.example.com"   # no :8080 if proxy is 443
register_token = "…"
tls_skip_verify = false                   # public CA
name = "prod-runner-01"
workspace_dir = "./workspace"
```

---

## Trust the certificate (production)

When `tls_skip_verify = false`, the Runner OS must trust Manager’s cert.

### Option 1 — Install Manager cert as trusted CA (self-signed lab → “trusted”)

Linux (Debian/Ubuntu):

```bash
sudo cp certs/cert.pem /usr/local/share/ca-certificates/buz-manager.crt
sudo update-ca-certificates
```

macOS:

```bash
sudo security add-trusted-cert -d -r trustRoot \
  -k /Library/Keychains/System.keychain certs/cert.pem
```

Then:

```toml
tls_skip_verify = false
manager_url = "https://buz.example.com:8080"
```

### Option 2 — Use a public CA (Let’s Encrypt, commercial)

No Runner OS change; keep `tls_skip_verify = false`.

---

## Config reference

| File | Key | Meaning |
|------|-----|---------|
| `manager.toml` `[server]` | `tls_cert` + `tls_key` | Both set → Manager serves HTTPS |
| `runner.toml` `[runner]` | `manager_url` | Must start with `https://` when Manager (or proxy) is HTTPS |
| `runner.toml` `[runner]` | `tls_skip_verify` | Skip cert verify (MITM risk — lab only) |

HTTPS is **not** configured at `cargo build` / Release / Docker **image build** time. Mount or place PEM files at deploy time.

---

## End-to-end checklist

```bash
# 1) Certs (Pattern A)
openssl x509 -in certs/cert.pem -noout -ext subjectAltName

# 2) Manager up on HTTPS
curl -k -sS -o /dev/null -w "%{http_code}\n" https://<manager-host>:8080/api/v1/health
# expect 200

# 3) UI
# open https://<manager-host>:8080/setup

# 4) Create Runner in UI → copy register_token into runner.toml

# 5) Runner
./bin/buz-runner -c conf/runner.toml
# UI → Runners → status online
```

---

## Troubleshooting

| Symptom | Check |
|---------|--------|
| Manager still HTTP | Both `tls_cert` and `tls_key` must be set and files readable from CWD |
| Browser `NET::ERR_CERT_COMMON_NAME_INVALID` | SAN / CN does not match the URL host; regenerate with correct `subjectAltName` |
| Runner `certificate verify failed` | Use matching hostname in `manager_url`, or install CA, or lab-only `tls_skip_verify = true` |
| Runner `Connection refused` | Firewall / wrong port; proxy 443 vs Manager 8080 |
| UI loads but WS logs fail behind Nginx | Ensure `Upgrade` / `Connection` proxy headers (see Pattern B) |

---

## Related

- [Configuration](/docs/03-operations/configuration/) — full `manager.toml` / `runner.toml`
- [Deployment](/docs/03-operations/deployment/) — Docker / binary packages
