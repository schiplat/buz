---
title: Ops Approval Switch
weight: 50
description: Require a second ops approval for host-config and resource-grant changes
aliases:
  - /docs/01-architecture/platform/approvals-switch/
---

Sensitive control-plane changes can be routed through a two-person approval
flow. The **Require approvals** toggle lives in the page header of
**Sandbox hosts** (`/runners/sandbox-hosts`) and is visible to platform
admins only.

## What it covers

Two independent switches exist in the backend; the UI presents one toggle
and writes both together:

| Backend key | What it queues |
|-------------|----------------|
| `sandbox_config_require_approval` | Host configuration documents (per-host config changes) |
| `grants_require_approval` | Resource grants and revocations (team × profile) |

The API keeps them independently settable (`PUT /api/v1/admin/settings/{key}`)
— the UI chooses not to expose that granularity.

## Semantics

- **On** — new host-config and grant/revoke changes are queued as pending
  changes; a second ops user must approve (the requesting user cannot
  approve their own change).
- **Off** — changes apply immediately.
- **Not retroactive.** Switching off does not auto-approve items already
  queued; they still require an explicit decision.

## Where approvals happen

Queued items appear in the **Grant approvals** card of the Sandbox hosts
page (Pending / History tabs). Pending rows show the request, requester and
time; the decision moment shows a live impact summary — for grants, the
pipelines the grant unlocks; for revocations, the queued jobs that would
fail at claim and the pipelines that lose access.

Every decision (who, when, from/to, impact counts) is written to the audit
log.
