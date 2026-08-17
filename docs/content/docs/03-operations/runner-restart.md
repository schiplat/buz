---
title: Runner Restart
weight: 5
description: "Remotely restart a Runner from the Manager UI"
---

Buz Runners are **pull-based**: they never listen for inbound commands. The
Manager therefore delivers a restart request over the Runner's existing 30s
heartbeat, and the Runner exits cleanly so an external supervisor brings it back
up. There is no self-update or in-process hot-reload — this is the only supported
"remote restart" path, by design.

## How it works

1. A platform admin clicks **Restart** on the Runners page (or calls the API).
2. The Manager sets a one-shot `restart_requested` flag on the runner row in the
   DB. It is persisted, so it survives a Manager restart.
3. Dispatch is blocked for that runner (same as `Freeze`) so no *pending* job is
   claimed while the restart is in flight.
4. On the runner's next heartbeat (≤30s), the Manager serves the flag and clears
   it. The runner then stops claiming new work, lets any *running* jobs finish,
   and exits with **code 0**.
5. The supervisor restarts the process.

```text
UI/API ──► Manager sets restart_requested=1 ──► heartbeat (≤30s) serves flag
                                                      │
                                                      ▼
                                      Runner drains running jobs, exit 0
                                                      │
                                                      ▼
                                supervisor (docker/systemd) restarts it
```

## Restart is refused while jobs are running

The Manager returns **409 Conflict** if the runner currently has `running` jobs,
listing their IDs. Cancel (or wait for) those jobs first, then retry.

`pending` jobs do **not** block the restart: dispatch is already stopped for the
runner once the flag is set, so those pending jobs are simply claimed by another
runner (or stay queued).

## Triggering a restart

**UI:** Runners → hover a runner → restart icon (⭯).

**API** (platform admin only):

```bash
curl -X POST http://127.0.0.1:8080/api/v1/runners/<id>/restart \
  -H "Authorization: Bearer $ADMIN_TOKEN"
```

## Supervisor requirements

The restart relies on an external supervisor to actually restart the process.
A runner run as a **bare foreground process will not come back on its own** —
you must restart it manually, or wrap it in a supervisor.

### Docker (`restart: unless-stopped` or `always`) — recommended

```yaml
services:
  runner:
    image: registry.example.com/buz/buz-runner:latest
    restart: unless-stopped
    # ... env / volumes ...
```

When the container's PID 1 (`buz-runner`) exits 0, Docker restarts the container.

### systemd (`Restart=always`)

```ini
[Service]
ExecStart=/opt/buz/bin/buz-runner -c /etc/buz/runner.toml
Restart=always
RestartSec=2
```

systemd restarts the service on a clean exit.

### Bare process (no supervisor)

Not auto-restarted. Run the runner under Docker/systemd, or plan a manual
`./buz-runner …` restart after the request.

## Timing and edge cases

| Case | Behavior |
|------|----------|
| Restart requested, no running jobs | Runner exits on the next heartbeat (~≤30s) and the supervisor restarts it |
| Restart requested, jobs still running | Runner drains them (status still reported), then exits |
| Runner is offline when requested | Flag persists in the DB; served on the next heartbeat after it comes back online |
| Runner re-registers before serving the flag | Registration clears `restart_requested` — a stale flag from a previous incarnation is never replayed on a freshly booted runner |
| Runner is frozen | Restart is independent of freeze; a frozen runner can still be restarted (its supervisor brings it back up, still frozen) |

## Events

Each phase is recorded on the runner's event log (Runners → runner → Events):

- `restart_requested` — the admin requested a restart.
- `restart_served` — the flag was delivered to the runner via heartbeat.
