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
- **Global coordination**: A single Center node orchestrates runners across regions, datacenters, and network segments

### Z — Zero (Zero Trust & Zero Burden)

Zero-trust security and architectural minimalism combined.

- **Zero-trust security**: Secrets stay in Runner memory only — never persisted, never sent back. Log masking via Aho-Corasick
- **Zero operations burden**: V1.x uses no Redis, MQ, or external DB. All persistence, scheduling, and services are built into the Center binary
