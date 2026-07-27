---
title: B-U-Z Principles
weight: 1
description: "The three core architectural principles of Buz"
---

## B — Binary

`buz-manager` and `buz-runner` compile to two entirely independent static binaries.

- Control plane (`buz-manager`) and execution agent (`buz-runner`) are fully decoupled
- Native Rust machine code — no JVM, no Node.js, no Python runtime
- Runner idle memory < 20MB, Center idle memory < 50MB
- Suitable for edge devices, low-spec servers, and constrained environments

## U — Unified

A single Center node orchestrates runners across all network segments globally.

- Standard HTTPS long polling — no gRPC, no WebSocket persistent connections
- Runners only need outbound HTTPS access to Center
- Perfectly penetrates enterprise firewalls, multi-layer proxies, and air-gapped networks
- Global coordination of distributed resources

## Z — Zero

Zero-trust security meets zero-operations burden.

- **Secrets**: Stored in Runner memory only — never on disk, never sent back to Center
- **Log masking**: Aho-Corasick multi-pattern matching with real-time secret redaction
- **No middleware**: No Redis, no MQ, no external database required for V1.x
- **Single binary**: Extract and run — zero configuration needed
