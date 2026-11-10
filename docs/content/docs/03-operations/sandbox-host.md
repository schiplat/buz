---
title: Sandbox Host Installation
weight: 35
description: "Install and verify a buz-sandboxd host: choose the tiers you need, install their runtimes, enroll the host, and confirm with buz-sandboxd doctor."
---

`buz-sandboxd` is the **optional** host supervisor that gives a Runner sandboxed
job tiers (`container`, `sandbox`, `vm`). You install it only if you want those
tiers; a Runner without it runs `host`-mode jobs exactly as before.

There is **no installer**. This page is the install procedure: pick the tiers
you need, install their prerequisites, enroll the host, then verify with
`buz-sandboxd --doctor`. Tiers are independent — install one now, add another
later without reinstalling sandboxd.

Architecture and trust model: [Sandbox Isolation Tiers](/docs/01-architecture/execution/sandbox-isolation-tiers/).
Policy model (profiles, tier images, network): [Sandbox Host Configuration](/docs/01-architecture/execution/sandbox-host-configuration/).

## 1. Choose the tiers you want

| Tier | Isolation boundary | Host prerequisites | Notes |
|------|--------------------|--------------------|-------|
| `container` (L1) | Hardened container (non-root, read-only root, dropped capabilities, no-new-privileges, resource limits) — **shares the host kernel** | Docker daemon | Cheapest; host-kernel attack surface remains |
| `sandbox` (L2) | Userspace application kernel (gVisor `runsc`) — job syscalls never reach the host kernel | Linux + `runsc` registered as a Docker runtime; **no `/dev/kvm`** | No hardware virtualization required |
| `vm` (L3) | MicroVM with its own guest kernel (Firecracker-compatible runtime, e.g. Kata `kata-fc`) | Linux + `/dev/kvm` + the runtime registered with Docker | Needs hardware virtualization |

L2 and L3 are **different risk shapes, not a total strength ordering**. Start
with `container` unless you specifically need the stronger boundary.

`host` (no sandbox) is not a sandboxd tier — it is what the Runner does without
sandboxd.

## 2. Install sandboxd

Two supported shapes; both use the same binary and the same bootstrap.

### Compose (recommended for a first host)

The base Compose file carries the supervisor under the `sandbox` profile; the VM
override exposes `/dev/kvm`:

```bash
# container / sandbox tiers
docker compose -f docker/docker-compose.yml --profile sandbox up -d

# ... add the vm tier (requires /dev/kvm on the host)
docker compose -f docker/docker-compose.yml -f docker/docker-compose.vm.yml \
  --profile sandbox up -d
```

### Binary + systemd

The release tarball ships `bin/buz-sandboxd`. Install it and a unit that starts
it as **root** (Docker socket access is host-root-equivalent by design):

```ini
# /etc/systemd/system/buz-sandboxd.service
[Unit]
Description=Buz sandbox host supervisor
After=docker.service
Requires=docker.service

[Service]
User=root
Environment=BUZ_SANDBOXD_ALLOWED_RUNNER_UID=10001
Environment=BUZ_MANAGER_URL=https://buz.example.com
Environment=BUZ_MANAGER_INSTANCE_ID=manager-xxxxxxxx
Environment=BUZ_SANDBOXD_TOKEN_FILE=/etc/buz/sandboxd.token
ExecStart=/usr/local/bin/buz-sandboxd
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
```

```bash
install -d -m 0700 /etc/buz
# token written during enrollment (step 4); then:
chmod 0400 /etc/buz/sandboxd.token
systemctl enable --now buz-sandboxd
```

## 3. Install the runtime for each tier

Only the **Buz-specific** parts are listed here; installing Docker, gVisor, or
Kata itself is their own upstream procedure (versions move quickly and distro
packages differ).

### `container` — Docker only

Nothing beyond a working Docker daemon. No daemon configuration change.

### `sandbox` — register gVisor `runsc` with Docker

1. Install `runsc` following the gVisor documentation.
2. Register it as a Docker runtime — merge this into `/etc/docker/daemon.json`
   (keep any existing keys):

   ```json
   {
     "runtimes": {
       "runsc": { "path": "/usr/local/bin/runsc" }
     }
   }
   ```

   Use the real path to the `runsc` binary if it differs.
3. Restart Docker and confirm it is listed:

   ```bash
   sudo systemctl restart docker
   docker info --format '{{json .Runtimes}}'
   ```

### `vm` — `/dev/kvm` + a Firecracker-compatible runtime

1. Confirm the device exists and the sandboxd process can read it:

   ```bash
   ls -l /dev/kvm
   ```

   On a virtual machine you need nested virtualization enabled. With Compose,
   the `docker-compose.vm.yml` override passes the device through; with a binary
   install, run sandboxd as root on a host that has the device.
2. Install a Firecracker-backed OCI runtime (for example Kata with its
   Firecracker configuration) and register it in `/etc/docker/daemon.json`
   under `runtimes` — same shape as `runsc`, with the path from that runtime's
   documentation.
3. Restart Docker and confirm the name appears in
   `docker info --format '{{json .Runtimes}}'`.

You can register both runtimes at once; each tier uses the runtime named in the
Manager's host document.

## 4. Enroll the host

Enrollment issues a one-time token scoped to `(manager_instance_id, host_id,
sandboxd_instance_id)`.

1. **Manager UI → Sandbox Hosts → Enroll**, or:

   ```bash
   POST /api/v1/ops/sandbox-hosts
   ```

   The token (`buzsd_…`) is shown **once**.
2. Store it root-owned on the host and point `BUZ_SANDBOXD_TOKEN_FILE` at it:

   ```bash
   install -m 0400 -o root -g root /path/to/buzsd_token /etc/buz/sandboxd.token
   ```

3. Set the same `BUZ_HOST_ID` you use for the Runner on this machine, and keep
   `BUZ_SANDBOXD_INSTANCE_ID` unique per Docker daemon.

## 5. Publish the host policy

sandboxd has **no local policy**: tier images, OCI runtimes, resource profiles,
and the job network come from the Manager. Until a document is applied,
`create_job` is refused while the health check still passes.

Publish from **Dashboard → Sandbox Hosts** (or `PUT
/api/v1/ops/sandbox-hosts/{host_id}/{instance_id}/config`):

- `default` profile must exist (memory / cpu / pids / slots)
- tier images must be pinned by digest (`…@sha256:…`)
- the image must provide `sh` and `timeout`
- `sandbox` / `vm` entries name the OCI runtime exactly as Docker reports it
  (`runsc`, `kata-fc`, …)

Details and the full document schema: [Sandbox Host Configuration](/docs/01-architecture/execution/sandbox-host-configuration/).

## 6. Verify

### Doctor (before or after enrollment)

`buz-sandboxd --doctor` is a read-only prerequisite report. It needs no token,
no Manager, and no applied policy, so run it as soon as the binary is on the
host:

```bash
buz-sandboxd --doctor            # human table
buz-sandboxd --doctor --json     # machine-readable
```

Exit codes: `0` at least one tier's prerequisites are satisfied; `2` none;
`1` a requested probe failed.

Doctor checks the host (OS, cgroup, `/dev/kvm`, Docker daemon and its runtimes)
and the bootstrap (token file mode, required UID, Manager URL), then prints one
line per tier with the reason it is blocked. The reasons use the same vocabulary
as the Manager's capability report, so `doctor` output and the UI badge tell the
same story.

To validate one **candidate image** end-to-end before publishing it, run the same
create → start → exec → remove sequence the Manager probe uses:

```bash
buz-sandboxd --doctor \
  --probe-image ghcr.io/example/job-image@sha256:… \
  --probe-runtime runsc
```

The image must be digest-pinned; the probe always removes the container.

### Service health and the authoritative check

```bash
systemctl status buz-sandboxd      # or: docker compose ... ps
buz-sandboxd --healthcheck
```

Then look at **Manager → Sandbox Hosts**: each configured tier shows
Supported/Unsupported plus the reason, and the Runner heartbeat carries the same
report. That probe is the authoritative verdict — doctor only checks
prerequisites, it cannot know the policy's images.

### One job per tier

Trigger a trivial pipeline on a Runner attached to this host with the tier's
`execution_mode` selected. Check the job log for the sandbox banner and the
step's output; a job that cannot get its tier fails explicitly rather than
falling back.

## 7. Troubleshooting

| Symptom (doctor / capability reason) | Cause | Fix |
|---|---|---|
| `docker daemon unreachable` | Docker not running, wrong CLI path, socket permissions | Start Docker; set `BUZ_SANDBOXD_DOCKER_CLI` to the real path |
| `runsc is not registered as a Docker runtime` | Installed but absent from `daemon.json`, or Docker not restarted | Add `runtimes.runsc`, restart Docker, recheck `docker info` |
| `no Firecracker-compatible runtime registered` | Kata/`kata-fc` missing or unregistered | Install and register it, restart Docker |
| `/dev/kvm missing or not readable` | No hardware virtualization, nested virt disabled, or device not passed through | Enable nested virtualization; with Compose use `docker-compose.vm.yml` |
| `sandbox runtime create failed: …` | Runtime name mismatch between host document and Docker | Make the document's runtime string identical to `docker info` |
| `image must provide sh and timeout` | Job image lacks a shell/`timeout` | Use an image that provides both (the probe fails closed) |
| `sandbox host has no applied configuration yet` | No host document published | Publish one in Dashboard → Sandbox Hosts |
| `token rejected` after rotation | Host still has the old token | Write the new token file and restart sandboxd |
| Health check passes but jobs are refused | Expected before a document is applied | Publish the document |
| `BUZ_SANDBOXD_ALLOWED_RUNNER_UID is unset` | sandboxd refuses to start without it | Set it to the Runner's UID |

## 8. Security notes

- sandboxd runs as **root** and drives the Docker socket. That is host-root
  equivalent: treat the host as a privileged service, not as an untrusted
  tenant boundary by itself.
- The Unix socket is the only channel the Runner uses; keep it group-restricted
  (`BUZ_SANDBOXD_SOCKET_GID` = the Runner's GID) and never expose it over the
  network.
- The enrollment token is a credential: `0400`, root-owned, and rotated through
  the ops API if leaked.
- `/dev/kvm` and the registered runtimes widen the kernel attack surface. Keep
  the host patched and use dedicated hosts for mutually untrusted teams.
- Jobs on one Runner sandbox host are *not* isolated from each other beyond the
  tier boundary — see the trust model in
  [SECURITY.md](https://github.com/schiplat/buz/blob/master/SECURITY.md).

## 9. Adding a tier later

1. Install the runtime (step 3) and restart Docker.
2. Re-run `buz-sandboxd --doctor` — the tier flips to `usable`.
3. Add that tier to the host document in the Manager (image + runtime).
4. Wait for the capability re-probe (5 minutes, or **Re-probe** in the UI) and
   select the new `execution_mode` for the Runner.

No sandboxd restart or reinstall is needed for a new tier beyond the Docker
runtime registration.

## 10. Removing sandboxd

```bash
systemctl disable --now buz-sandboxd        # binary install
docker compose -f docker/docker-compose.yml --profile sandbox down   # Compose
```

Leftover job containers carry the `buz.sandboxd.instance=<id>` label and are
force-removed on the next start of the same instance; other instances'
containers are never touched. The token can be revoked from the ops API.
