---
title: Core Concepts
weight: 20
description: The ten-minute mental model — Manager, Runner, Hosts, plugins, teams
---

Buz is a cross-network orchestration control plane: it runs pipelines on
machines it does not share a network with.

## The four moving parts

| Part | What it is | Runs where |
|------|-----------|------------|
| **Manager** | Control plane — stores pipelines, queues jobs, exposes the API + dashboard | Your operations network |
| **Runner** | Execution agent — polls the Manager, claims jobs, runs steps | Any machine that can reach the Manager (HTTP(S) outbound) |
| **Hosts** | SSH targets a Runner deploys to over its own credentials | Any machine the Runner can SSH to |
| **sandboxd** | Optional host supervisor for sandboxed job tiers — [install & verify](../03-operations/sandbox-host.md) | Docker-capable hosts |

## A pipeline

A pipeline is YAML: inputs, steps, and where steps run. Steps are either
inline `run:` scripts or `uses:` references to plugin templates from the
[marketplace](https://plugins.buz.schiplat.com) — resolved and
content-hash-locked at save time.

## Secrets

Secrets are team-scoped, encrypted at rest (AES-256-GCM, keyed by the
Manager's `jwt_secret`), and referenced GHA-style: `${secrets.NAME}` at a
use site allowlists and binds the value — nothing is auto-injected into the
process environment. See [Secrets & Variables](/docs/01-architecture/authoring/secrets-variables/).

## Governance

Teams isolate resources (RBAC); environments gate deployments; the
**Require approvals** switch queues sensitive changes (host config,
resource grants) for a second ops approval. Admin actions are audited.

## Where to next

- Author your first pipeline: [Pipeline Format](../01-architecture/authoring/pipeline-format.md)
- Deploy something: [Deployment](../03-operations/deployment.md)
- Add sandboxed job tiers: [Sandbox Host Installation](../03-operations/sandbox-host.md)
- How those tiers differ: [Sandbox Isolation Tiers](../01-architecture/execution/sandbox-isolation-tiers.md)
- Operating safely: [Security policy](https://github.com/schiplat/buz/security/policy)
