---
title: Architecture
weight: 1
description: "Buz platform architecture — B-U-Z core principles and system design"
---

## B-U-Z Architecture Principles

Buz's architecture strictly follows three core principles, which define the very foundation of the platform:

### B — Binary (Independent Native Binary)

Center and Runner compile as two fully standalone static binaries, completely eliminating runtime dependencies and redundant packaging.

- **Decoupled roles**: `buz-manager` (control plane) and `buz-runner` (execution agent) — clear separation of concerns, independently deployable
- **Ultra-lightweight**: Native Rust binaries. Runner idle < 20MB, Center idle < 50MB

### U — Unified (Unified Orchestration Model)

Designed for heterogeneous networks, cross-datacenter, and multi-region environments.

- **NAT traversal**: HTTP long polling — Runner only needs outbound access to Center, no public IP or inbound ports required
- **Global coordination**: A single Manager orchestrates runners across regions, datacenters, and network segments

### Z — Zero (Zero Trust & Zero Burden)

Zero-trust security and architectural minimalism combined.

- **Zero-trust security**: Secrets stay in Runner memory only — never persisted, never sent back. Log masking via Aho-Corasick
- **Zero operations burden**: V1.x uses no Redis, MQ, or external DB. All persistence, scheduling, and services are built into the Center binary

## Key topics

{{< cards >}}
  {{< card link="runner-concurrent-jobs/" title="Runner Policy" subtitle="UI-managed concurrency, engines, cleanup" >}}
  {{< card link="docker-engine/" title="Docker Engine" subtitle="uses: docker + registry_secret" >}}
  {{< card link="secrets-access-control/" title="Secrets Access Control" subtitle="kind, team_shared, bindings, registry_secret" >}}
  {{< card link="env-render/" title="Env Render" subtitle="ValueMap, ${secrets.X}, BUZ_ENV_CHANGED" >}}
  {{< card link="config-block/" title="Config Block DSL v2" subtitle="Agreed: nested config, OSS + render-only" >}}
  {{< card link="pipeline-format/" title="Pipeline Format" subtitle="YAML fields including registry_secret" >}}
  {{< card link="job-concurrency-queue/" title="Job Concurrency Queue" subtitle="Pipeline-level queue / discard" >}}
  {{< card link="git-webhook-triggers/" title="Git Webhook Triggers" subtitle="GitHub/Gitee push, secret verify, Trigger events" >}}
  {{< card link="admin-audit-logs/" title="Admin Audit Logs" subtitle="Platform-admin mutation audit + redaction" >}}
  {{< card link="api-reference/" title="API Reference" subtitle="HTTP endpoints including Runners" >}}
{{< /cards >}}
