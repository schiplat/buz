---
title: TLS (HTTPS)
weight: 3
description: "Enable HTTPS on Manager and connect Runners — including self-signed certificates"
---

# TLS (HTTPS)

Manager can serve the API and Web UI over HTTPS. Runners are HTTPS **clients** (they do not expose a TLS server).

## Self-signed certificate (lab / private network)

### 1. Generate cert + key

On the Manager host:

```bash
mkdir -p certs
openssl req -x509 -newkey rsa:4096 -nodes \
  -keyout certs/key.pem \
  -out certs/cert.pem \
  -days 365 \
  -subj "/CN=localhost"
```

If clients will connect by IP or hostname, add a SAN (OpenSSL 3 example):

```bash
openssl req -x509 -newkey rsa:4096 -nodes \
  -keyout certs/key.pem \
  -out certs/cert.pem \
  -days 365 \
  -subj "/CN=buz.local" \
  -addext "subjectAltName=DNS:buz.local,DNS:localhost,IP:127.0.0.1"
```

### 2. Manager config

Paths are relative to the Manager working directory (e.g. `example/`):

```toml
[server]
host = "0.0.0.0"
port = 8080
tls_cert = "certs/cert.pem"
tls_key  = "certs/key.pem"
```

Restart Manager. Open `https://localhost:8080` — browsers warn on self-signed certs; that is expected.

### 3. Runner config

```toml
[manager]
url = "https://127.0.0.1:8080"   # must be https
register_token = "…"
tls_skip_verify = true            # lab only: accept self-signed Manager cert
```

**Production:** keep `tls_skip_verify = false` and trust the certificate (install your CA / Manager cert into the Runner OS trust store), or use a publicly trusted cert.

| Setting | Meaning |
|---------|---------|
| Manager `tls_cert` + `tls_key` | Enables HTTPS when **both** are set |
| Runner `manager.url` | Must use `https://` when Manager has TLS |
| Runner `tls_skip_verify` | Skips cert verification (MITM risk — labs only) |

## Notes

- Only Manager needs certificates; Runner outbound HTTPS is enough for B-U-Z NAT-friendly design.
- Certificate `CN` / SAN should match the host name or IP in the Runner URL (unless `tls_skip_verify`).
- Frontend static files are served by the same Manager listener — one HTTPS endpoint covers UI + API.

See also: [Configuration](/docs/03-operations/configuration/), [Deployment](/docs/03-operations/deployment/).
