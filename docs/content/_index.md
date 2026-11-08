---
title: "Documentation"
type: docs
weight: 1
description: "Buz runs YAML pipelines across firewalled networks — the Manager is the only component that listens. GitHub Actions ergonomics, Jenkins-grade governance, one binary per role."
---

Buz runs YAML pipelines on machines across firewalls, NATs, and network
segments — machines that were never meant to talk to each other. The
**Manager** is the only component that listens; **Runners** connect
outbound-only, so targets need zero inbound ports, no VPN, no port
forwarding. Authoring follows GitHub Actions (declarative YAML, `uses:`
steps, GHA-style secrets); governance follows Jenkins (approval gates, RBAC,
audit logs); and plugin references are locked to a content hash at save
time — primitives that make Buz a safe target for AI-agent-authored
automation. This site documents how to evaluate, operate, and extend it.

{{< callout type="tip" >}}
**New here?** The [5-minute standalone demo](/docs/00-quick-start/) runs the
whole stack - Manager + Runner - in one container. No toolchain required.
{{< /callout >}}

## Start by goal

| You want to... | Go to |
|---|---|
| See it running in 5 minutes | [Quick Start](/docs/00-quick-start/) |
| Understand the design | [Core Concepts](/docs/00-quick-start/core-concepts/) - [Principles](/docs/01-architecture/principles/) |
| Write pipelines and use plugins | [Pipeline Format](/docs/01-architecture/authoring/pipeline-format/) - [Secrets](/docs/01-architecture/authoring/secrets-access/) - [Plugin Contract](/docs/01-architecture/authoring/plugin-contract/) |
| Deploy a single-box lab | [Standalone](/docs/03-operations/standalone/) |
| Operate in production | [Deployment](/docs/03-operations/deployment/) - [Configuration](/docs/03-operations/configuration/) |
| Rotate keys, scan legacy config | [Key Rotation](/docs/03-operations/key-rotation/) - [CLI Reference](/docs/03-operations/cli-reference/) |
| Control access and approvals | [RBAC](/docs/01-architecture/platform/rbac/) - [Approvals Switch](/docs/01-architecture/platform/approvals-switch/) - [Audit Logs](/docs/01-architecture/platform/admin-audit-logs/) |
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
