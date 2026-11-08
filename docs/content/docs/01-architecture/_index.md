---
title: Architecture
weight: 2
description: "Buz platform architecture — principles, then authoring → execution → platform"
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

More: [Principles](./principles/)

---

## Reading map (recommended)

Follow the product journey — **run locally → write a pipeline → see how it runs → operate**:

| Stage | Section | Start here |
|-------|---------|------------|
| **1. Quick Start** | [Quick Start](/docs/00-quick-start/) | `make run` · then [Deployment](/docs/03-operations/deployment/) |
| **2. Concepts** | This page | Principles above |
| **3. Authoring** | [Authoring](./authoring/) | [Pipeline Format](./authoring/pipeline-format/) (parameter reference) |
| **4. Execution** | [Execution](./execution/) | [SSH Multi-Host](./execution/ssh-multi-host/) · [Docker Engine](./execution/docker-engine/) |
| **5. Platform** | [Platform](./platform/) | [RBAC](./platform/rbac/) · [Secrets Access Control](./platform/secrets-access-control/) |
| **6. Operations** | [Operations](/docs/03-operations/) | Configuration · Deployment · Standalone · TLS |
| **7. Development** | [Development](/docs/02-development/) | Schema · project structure (contributors) |

---

## Sections

{{< cards >}}
  {{< card link="authoring/" title="Authoring" subtitle="YAML format · expressions · config · notify · triggers" >}}
  {{< card link="execution/" title="Execution" subtitle="Runner policy · Docker · SSH multi-host" >}}
  {{< card link="platform/" title="Platform" subtitle="RBAC · secrets · API · audit" >}}
  {{< card link="principles/" title="Principles" subtitle="B-U-Z detail notes" >}}
{{< /cards >}}
