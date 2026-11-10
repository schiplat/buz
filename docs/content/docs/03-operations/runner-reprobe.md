---
title: Runner Capability Re-probe
weight: 85
description: "On-demand sandbox capability re-probe from the Manager UI, and how the lifecycle shows up in the Runner drawer"
---

Sandbox capability checks are periodic (the Runner re-probes its tiers every
5 minutes and after a Runner restart). The Manager UI additionally offers an
**on-demand re-probe** — same heartbeat-delivery model as
[Runner Restart](runner-restart.md): Runners are pull-based and never listen
for inbound commands, so the request rides the Runner's existing ≤30s
heartbeat.

## Request lifecycle

1. **Requested** — the admin clicks *Re-probe* in the Runner drawer
   (Runners → detail → *Sandbox environment checks*). The Manager sets
   `capability_reprobe_requested` and stamps `capability_reprobe_requested_at`.
   The request is **saved server-side**: if the Runner is offline it runs on
   the first heartbeat after reconnecting.
2. **Served** — the next heartbeat delivers the command and the Manager stamps
   `capability_reprobe_served_at`. The Runner starts probing immediately.
3. **Results** — probing runs a short-lived workload per tier
   (container → start → exec → remove; tens of seconds to ~1–2 min in total),
   then reports fresh statuses with the following heartbeat.

The Runner drawer derives its banner from this state: **Queued** (waiting for
the heartbeat, amber) → **Probing** (blue, with a live timer) → **Done**
(green). While queued or probing the drawer polls the single-runner endpoint
every 5s; a 3-minute watchdog stops the polling and surfaces a
"check the Runner's log" hint instead of spinning forever.

## Reading the results

Each tier (L1 `container`, L2 `sandbox`, L3 `vm`) reports
`supported / unsupported` plus the delivering OCI runtime
(`sandbox → runsc`, `vm → kata-fc`, …). Results are stamped with a
`checked_at` timestamp; statuses older than 5 minutes are shown as `unknown`
so a stale "supported" never hides a broken backend. See
[Isolation tiers](../01-architecture/execution/sandbox-isolation-tiers.md) for
what each tier guarantees.

To diagnose a host *before* it is enrolled (no token, no Manager, no applied
policy), run `buz-sandboxd --doctor` on it — the same prerequisite rules, one
line per tier with the blocking reason. See
[Sandbox Host Installation](./sandbox-host.md#6-verify).

Both the request and the serve are recorded in the Runner's event log and the
Manager's audit trail.
