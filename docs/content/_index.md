---
title: "Documentation"
type: docs
weight: 1
description: "Buz — a self-hosted pipeline platform: one static Rust binary per role, declarative YAML with GitHub Actions ergonomics and Jenkins-grade governance, a full stack in one container in five minutes."
---

Buz is a self-hosted pipeline platform that ships as a single static Rust
binary per role — no JVM, no runtime dependencies — and a full Manager +
Runner stack comes up in one container in five minutes.

Everything is declared in YAML, not configured in a UI: pipelines, triggers,
environments, concurrency queues, retries, and per-step timeouts.

Steps are shell, SSH, Docker, or `uses:` references into a reviewed plugin
marketplace, locked to a content hash at save time; secrets follow the
GitHub Actions `${secrets.NAME}` style and are never auto-injected into the
process environment. Runners connect outbound-only, so the machines doing
the work keep zero inbound ports.

Governance is built in — environments with approval gates, RBAC, and audit
logs keep a human in charge of production deploys; the same primitives make
Buz a safe target for AI-agent-authored automation. This site documents how
to evaluate, operate, and extend it.

In its design, Buz borrows proven ideas from GitHub Actions and Jenkins.

{{< callout type="tip" >}}
**New here?** The [5-minute standalone demo](/docs/00-quick-start/) runs the
whole stack - Manager + Runner - in one container. No toolchain required.
{{< /callout >}}

## Start by goal

| You want to... | Go to |
|---|---|
| See it running in 5 minutes | [Quick Start](/docs/00-quick-start/) |
| Understand the design | [Core Concepts](/docs/00-quick-start/core-concepts/) - [Principles](/docs/01-architecture/principles/) |
| Write pipelines and use plugins | [Pipeline Format](/docs/01-architecture/authoring/pipeline-format/) - [Secrets & Variables](/docs/01-architecture/authoring/secrets-variables/) - [Plugin Contract](/docs/01-architecture/authoring/plugin-contract/) |
| Deploy a single-box lab | [Standalone](/docs/03-operations/standalone/) |
| Operate in production | [Deployment](/docs/03-operations/deployment/) - [Configuration](/docs/03-operations/configuration/) |
| Rotate keys, scan legacy config | [Key Rotation](/docs/03-operations/key-rotation/) - [CLI Reference](/docs/03-operations/cli-reference/) |
| Control access and approvals | [RBAC](/docs/01-architecture/platform/rbac/) - [Ops Approval Switch](/docs/01-architecture/platform/ops-approval-switch/) - [Audit Logs](/docs/01-architecture/platform/admin-audit-logs/) |
| Report a vulnerability | [Security Policy](https://github.com/schiplat/buz/security/policy) |

## Sections

{{< cards >}}
  {{< card link="/docs/00-quick-start/" title="Quick Start" subtitle="Local run - demo - standalone - migration" >}}
  {{< card link="/docs/01-architecture/" title="Architecture" subtitle="Principles - Authoring - Execution - Platform" >}}
  {{< card link="/docs/02-development/" title="Development" subtitle="Project structure - database schema" >}}
  {{< card link="/docs/03-operations/" title="Operations" subtitle="Configuration - Deployment - CLI - Key rotation" >}}
  {{< card link="/docs/releases/" title="Releases" subtitle="Release notes - migration guides" >}}
{{< /cards >}}

## Elsewhere

- [Plugin Marketplace](https://plugins.buz.schiplat.com/) - browse official step templates
- [GitHub](https://github.com/schiplat/buz) - source, issues, releases
- [Standalone image](https://hub.docker.com/r/schiplat/buz-standalone) - schiplat/buz-standalone on Docker Hub
