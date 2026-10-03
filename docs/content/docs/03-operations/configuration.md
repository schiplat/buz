---
title: Configuration Reference
weight: 1
description: "All configuration options for Manager and Runner"
---

## Manager (`conf/manager.toml`)

```toml
[server]
host = "0.0.0.0"
port = 8080
# tls_cert = "certs/cert.pem"   # both set → HTTPS
# tls_key  = "certs/key.pem"

[database]
path = "./data/buz.db"
max_connections = 8

[auth]
jwt_secret = ""                     # required — or set BUZ_JWT_SECRET (openssl rand -hex 32)
# Local/demo with a weak template secret only: BUZ_ALLOW_INSECURE_JWT=1
jwt_ttl_hours = 24
runner_register_token_ttl_hours = 24

[runner]
heartbeat_timeout_seconds = 90

[pipeline]
yaml_scan_dir = "./pipelines"
yaml_scan_interval_seconds = 30

[clock]
enabled = true                 # probe remote clock before signing config URLs
warn_threshold_seconds = 60    # log-only warning threshold
cache_ttl_seconds = 300        # reuse a probe result for this long
# allowed_hosts = ["*.aliyuncs.com"]   # hosts the guard may probe (SSRF allowlist)

# Dev / single-container only — leave disabled in production (default)
# [standalone]
# enabled = true
# auto_approve = ["standalone-runner"]

[logging]
level = "info"                 # error | warn | info | debug | trace
# file = "./logs/manager.log"  # optional; when set → stdout + file
```

| Section | Key | Notes |
|---------|-----|-------|
| `server` | `host` / `port` | Listen address |
| `server` | `tls_cert` + `tls_key` | Optional HTTPS (both required). See [TLS](/docs/03-operations/tls/) |
| `database` | `path` | SQLite file |
| `auth` | `jwt_secret` | Signs JWTs + encrypts secrets/hosts. **Required** (≥ 32 chars). Empty / `CHANGE_ME` / `changeme` / `demo-only` placeholders are **refused at startup** unless `BUZ_ALLOW_INSECURE_JWT=1` (local only) |
| `auth` | `jwt_ttl_hours` | UI/API JWT lifetime |
| `auth` | `runner_register_token_ttl_hours` | Runner JWT lifetime after register |
| `runner` | `heartbeat_timeout_seconds` | Offline threshold for runners |
| `pipeline` | `yaml_scan_*` | File/Git authority for pipeline YAML; scanner + UI online edit write here. **Flat directory only** (basename files; no private subdirs). See [YAML-First Authoring](/docs/01-architecture/authoring/yaml-first-authoring/) |
| `clock` | `enabled` | Default `true`. Probe the OSS endpoint's `Date` header before signing `config.source` URLs — see [Clock skew](#clock-skew) |
| `clock` | `warn_threshold_seconds` | Default `60`. Log a `warn` when the Manager clock is at least this far behind remote time. `0` disables the warning |
| `clock` | `cache_ttl_seconds` | Default `300`. Reuse a probe result for this long; `0` probes on every dispatch |
| `clock` | `allowed_hosts` | Default `["*.aliyuncs.com"]`. Hosts the guard may probe. The probe target is derived from a team-scoped `config_source` secret, so this is the **SSRF allowlist**: a host outside it is never contacted (the guard is skipped for that endpoint). Extend it for S3-compatible endpoints (MinIO etc.); name a port explicitly (`minio.internal:9000`) to allow a non-443 endpoint. An empty list disables probing. Redirects are never followed |
| `standalone` | `enabled` | Default `false`. When `true`, allow **loopback** Runner register without `register_token` for names in `auto_approve` (or pre-created in UI). Non-loopback register is **rejected**. Dev / `buz-standalone` only — see [Standalone mode](#standalone-mode) |
| `standalone` | `auto_approve` | Runner names created at Manager boot if missing. Default `["standalone-runner"]` |
| `logging` | `level` | Default `info`. Overridden by `RUST_LOG` if set |
| `logging` | `file` | Optional append path; empty = stdout only |

**First admin:** not in config. On a fresh database, open the UI — you are redirected to `/setup` to create the platform admin (password ≥ 8 chars, complexity rules apply). After that, use `/login`.

**Password storage:** login passwords use **Argon2id** in `users.password_hash`. This is separate from Secrets/Hosts **AES-256-GCM** encryption. A login failure is almost always a wrong password (or wrong username), not a crypto mismatch.

**Forgot admin password (local/dev recovery):** stop Manager, then from a machine with the Argon2 tooling available set a new hash, e.g. generate one via a one-liner and:

```sql
UPDATE users SET password_hash = '<argon2id-phc-string>' WHERE username = 'admin';
```

Then restart Manager and sign in. Prefer changing the password again from the UI after recovery.

## Runner (`conf/runner.toml`)

Machine-local only. Strategy is **not** configured here — see [Runner Policy](/docs/01-architecture/execution/runner-concurrent-jobs/).

| Managed in UI | Kept in TOML (`[runner]` / `[engine]` / `[logging]`) |
|---------------|-----------------------------------------------------|
| `labels` | `manager_url`, `register_token`, `tls_skip_verify`, `standalone` |
| `max_concurrent_jobs`, cleanup | `name`, `workspace_dir`, `token_cache_path` |
| engine enable flags + `ssh_host_key_policy` | `prefer_interfaces`, `ssh_known_hosts_path` |

```toml
[runner]
name = "prod-runner-01"              # must match UI Runner name
manager_url = "https://buz-api.example.com"
register_token = "paste-from-ui"     # or env BUZ_REGISTER_TOKEN
# standalone = false                 # true → allow empty register_token (lab image only)
# tls_skip_verify = false
workspace_dir = "/tmp/buz-runner"
token_cache_path = "./state/runner-token" # durable Manager-issued JWT cache
# prefer_interfaces = ["enp26s0"]

[engine]
# ssh_known_hosts_path = "/home/runner/.ssh/known_hosts"

[clock]
enabled = true                 # validate this host's clock against the Manager
warn_threshold_seconds = 60    # warn when this Runner drifts this far
max_skew_seconds = 300         # gross skew → refuse to start (NTP required)
# fail_on_skew = true          # false → log an error and start anyway (labs)

[logging]
level = "info"
# file = "./logs/runner.log"   # Runner default if omitted: ./logs/runner.log
```

| Key | Notes |
|-----|-------|
| `register_token` | Required unless `standalone = true` (see [Standalone mode](#standalone-mode)) |
| `standalone` | Default `false`. When `true`, Runner may start with an empty `register_token` and register over loopback against a Manager with `[standalone] enabled = true` |
| `token_cache_path` | Default `./state/runner-token`. Writable, durable path for the Manager-issued JWT; created with Unix mode `0600`. `BUZ__RUNNER__TOKEN_CACHE_PATH` overrides it. The old `<workspace_dir>/runner-token` is read only when the new path is missing and removed after a successful cache write. |
| `clock.enabled` | Default `true`. Validate the Runner clock against the Manager — see [Clock skew](#clock-skew) |
| `clock.warn_threshold_seconds` | Default `60`. Log a `warn` once per drifted minute above this offset |
| `clock.max_skew_seconds` | Default `300`. Skew at/above this is treated as a broken clock at startup |
| `clock.fail_on_skew` | Default `true`. `true` → startup aborts with the skew and the NTP command; `false` → `error` log and continue (labs). Mid-run drift is always logged/reported, never fatal |

> Obsolete `[manager]` section is **rejected** at startup with a migration hint.

## Standalone mode

**Dev / lab only** — same-container Manager + Runner (`buz-standalone` image), or a local loopback pair. Do **not** enable on a production multi-runner control plane.

### Manager — `[standalone]`

```toml
[standalone]
enabled = true
auto_approve = ["standalone-runner"]   # created at boot if missing
```

| Behavior | Detail |
|----------|--------|
| Boot | For each name in `auto_approve`, create an authorized runner row if absent (`labels` include `standalone`) |
| Register (loopback) | Peer `127.0.0.1` / `::1` may register **without** `register_token` when the runner name is authorized (`auto_approve` or UI **New Runner**) |
| Register (remote) | Always **rejected** while `enabled = true` — even with a valid token |
| Default | `enabled = false` — normal token registration only |

Checked-in template: `deploy/conf/manager.standalone.toml`.

### Runner — `[runner].standalone`

```toml
[runner]
name = "standalone-runner"           # must match auto_approve / UI name
manager_url = "http://127.0.0.1:8080"
standalone = true
register_token = ""                  # allowed when standalone = true
```

| Behavior | Detail |
|----------|--------|
| Startup | Empty `register_token` is allowed only when `standalone = true`; otherwise Manager refuses to start the process |
| Pairing | Expects Manager `[standalone] enabled = true` and loopback `manager_url` |

Checked-in template: `deploy/conf/runner.standalone.toml`.

First-run Compose, volumes, and upgrade: **[Standalone (single image)](/docs/03-operations/standalone/)**.

## Clock skew

**Keep every host clock synchronized (NTP): the Manager and every Runner.** Both sides validate the
offset against the other and report it; neither ever changes the system clock itself.

### Runner ↔ Manager (`[clock]` on the Runner)

The Runner must stay consistent with the **Manager** clock, so it measures the offset against the
Manager's `Date` header (the unauthenticated `GET /health` endpoint):

| When | Behavior |
|------|----------|
| **Startup** (before registering) | Skew < `warn_threshold_seconds` → `info` log with the measured offset; ≥ threshold → `warn`; ≥ `max_skew_seconds` → **refuses to start** (`fail_on_skew = true`, the default) with the offset, the limit and the NTP command |
| **Every heartbeat** (30s) | Same measurement taken **from the heartbeat response** — no extra request; a drift above the threshold logs once per minute bucket instead of every 30s |
| Probe unavailable (Manager unreachable, no `Date`) | `warn`/`debug` log, Runner continues — the check is a guard, not a dependency |

The measured skew is reported to the Manager in `system_info.clock_skew_seconds`
(`Manager - Runner`; positive = the Runner is behind) and shown in **Runners → runner detail** as a
`Clock +Ns vs Manager` chip, amber at ≥ 60s and red at ≥ 300s.

```log
# Healthy startup
clock: Runner clock consistent with Manager  source=https://buz-api.example.com/health skew=+2s behind

# Drifting host
clock: Runner clock drifts from Manager — check NTP on this host  skew_secs=125

# Broken clock → refused to start
ERROR clock: Runner clock too far from Manager — refusing to start  skew_secs=9600 max_skew_seconds=300
```

Mid-run drift is **reported, not fatal**: a Runner that is already executing jobs keeps working
(aborting mid-flight would be worse than off-timeline timestamps); the Manager UI and logs surface it.

### Manager → OSS (`[clock]` on the Manager)

`config.source` delivery hands the Runner a
**pre-signed OSS URL** whose `Expires` is an absolute Unix timestamp computed from the Manager's own
clock (`crates/shared/src/config_auth.rs`). If the Manager clock has drifted **behind** real time,
the URL is already expired the moment it is signed and every config-delivery step fails with:

```text
ERROR config bundle download returned HTTP 403 Forbidden: AccessDenied: Request has expired.
```

A bare `403` like the above points at nothing — the URL looks valid, only OSS disagrees. Runner and DB
host clocks do **not** affect this check (OSS validates against its own time).

**The job log now names the cause itself**, so a `403` is not a dead end:

```text
==> CONFIG PREP
  expires_in       3600s
  signed_at        2026-09-20 10:14:35 UTC
  url_age          2s
  url_remaining    3598s
```

`signed_at` / `url_age` / `url_remaining` are stamped by the Manager at signing time
(`signed_at_epoch` / `expires_epoch` in `config_auth`), and a URL that arrives **already expired**
adds an explicit `url_warning`. Combined with the OSS `Date` header reported on download failure,
the two possible causes are distinguishable without guessing:

- `url_age` small (seconds) + expired → the **Manager clock lagged** at signing time;
- `url_age` ≳ TTL → the **task waited too long** between claim and download (clocks may be fine).

```text
config bundle download returned HTTP 403 Forbidden: AccessDenied: Request has expired.
  oss_time     2026-09-20 12:54:56 UTC
  local_time   2026-09-20 11:14:37 UTC
  clock_skew   6000s behind (this host is behind OSS — sync NTP on the runner host)
```

### Guard (`[clock]`)

Before signing, the Manager issues a `HEAD` to `https://{bucket}.{endpoint}/` and reads the `Date`
response header (present even on a `403`), then compares it with the local clock:

| Situation | Behavior |
|-----------|----------|
| Skew below `warn_threshold_seconds` | `debug` log, URL signed |
| Skew at/above `warn_threshold_seconds` | `warn` log naming the source, URL still signed |
| Skew so large the URL would arrive expired (≈ TTL minus a 300s margin) | URL **not** signed; the step fails with an actionable `config_auth_error` naming the clock, the measured skew and the NTP command |
| Probe fails (DNS/TLS/timeout) | `warn` log, URL signed (the guard never blocks dispatch on its own failure) |
| Target host not in `clock.allowed_hosts` | Probe **not attempted** (`debug` log), URL signed — the guard is skipped |
| Target port is neither 443 nor named in `clock.allowed_hosts` | Probe **not attempted**, URL signed — an allowlisted host must not become a port scanner |

**The target host is attacker-influenced**, so the guard treats it as untrusted input: it comes from
a team-scoped `config_source` secret (`https://{bucket}.{endpoint}/`), and any user who can manage a
secret in their team could otherwise aim the Manager's HTTP client at an internal host or a cloud
metadata endpoint. Therefore the guard dials **only** hosts matching `clock.allowed_hosts`
(default `*.aliyuncs.com`), only on port 443 unless an entry names another port (e.g.
`minio.internal:9000`), never follows redirects, and rejects a source carrying userinfo
(`user@host`), a path or non-DNS characters before any request is made. Endpoints outside the
allowlist simply lose the clock guard — add them to get it back.

`/health` reports the last probe:

```json
"clock": { "status": "ok", "skew_seconds": 0, "behind_seconds": 0, "checked_at": "…", "probe_failed": false }
```

`status` is `ok` | `skewed` | `unknown` (`unknown` = no probe yet, e.g. nothing signed since boot).

`/health` is **unauthenticated**, so this payload deliberately omits the probe source
(`https://{bucket}.{endpoint}/`) and the raw probe error: both would disclose internal OSS endpoints
and hostnames to anonymous callers. Operators get that detail from the Manager logs.

### Fix

```bash
# On the affected host (Manager and/or Runner)
sudo timedatectl set-ntp true
sudo systemctl enable --now systemd-timesyncd   # or: systemctl restart chrony && chronyc makestep
sudo hwclock --systohc
timedatectl status                              # System clock synchronized: yes
```

Confirm the skew directly at any time:

```bash
# Manager ↔ OSS
date -u
curl -sI https://<bucket>.<endpoint>/ | grep -i '^date:'

# Runner ↔ Manager
date -u
curl -sI https://<manager>/health | grep -i '^date:'
```

`GET /health` on the Manager also reports its own last OSS probe under `clock`.

Disable the guard only if the probe is unusable in your environment (`[clock] enabled = false` or
`BUZ__CLOCK__ENABLED=false`) — the underlying clock problem remains.

## Logging

| Destination | Behavior |
|-------------|----------|
| stdout | Always on (Docker: `docker logs`) |
| Runner file | **Default** `./logs/runner.log` (Docker WORKDIR → `/app/buz/logs/runner.log`) |
| Manager file | Optional via `logging.file` (no default file) |
| Level | `logging.level` (default `info`); `RUST_LOG` overrides |

Set `logging.file = ""` (or `"-"` / `"none"`) on Runner for **stdout-only**.

Examples:

```bash
# Config file
# [logging]
# level = "debug"
# file = "/app/buz/logs/manager.log"

# One-shot override
RUST_LOG=buz_manager=debug,sqlx=warn ./buz-manager -c conf/manager.toml
```

Do **not** log secrets (`register_token`, JWT, passwords). Create-runner API returns the token once in the JSON body; process logs only record name/id.

## Environment overrides

| Variable | Effect |
|----------|--------|
| `BUZ_JWT_SECRET` | Overrides `auth.jwt_secret` (required for packaged/Docker deploys) |
| `BUZ_ALLOW_INSECURE_JWT` | `1` / `true` — allow empty or placeholder JWT secrets (**local/demo only**) |
| `BUZ_MANAGER_URL` | Overrides `runner.manager_url` |
| `BUZ_REGISTER_TOKEN` | Overrides `runner.register_token` |
| `BUZ__RUNNER__TOKEN_CACHE_PATH` | Overrides `runner.token_cache_path` |
| `RUST_LOG` | Overrides `logging.level` filter |
| `BUZ__…` | Nested TOML via `config` crate (`BUZ__SERVER__PORT=8081`) |

Priority: CLI args > environment > config file > defaults.

> There is **no** `env.VAR_NAME` interpolation inside TOML strings. Use the variables above (or `BUZ__` nested keys).
