---
title: Execution
weight: 20
description: "How the Runner executes steps — runner policy, Docker, SSH multi-host, sandbox isolation"
---

## Who should read this

You operate **Runner machines**, or you need to understand how a claimed job actually executes: engines, fan-out, isolation tiers, and per-machine policy.

## Reading order

1. [Runner Policy](./runner-policy/) — UI-managed concurrency, engines, cleanup, host keys
2. [Docker Engine](./docker-engine/) — `engine: docker`, workspace mount, private-registry login
3. [SSH Multi-Host](./ssh-multi-host/) — chain default · parallel opt-in · cancel-all
4. [Sandbox Isolation Tiers](./sandbox-isolation-tiers/) — host → container → sandbox → vm
5. [Sandbox Host Configuration & Resource Profiles](./sandbox-host-configuration/) — Manager-managed config, named profiles

Authoring fields live in [Pipeline Format](/docs/01-architecture/authoring/pipeline-format/). Ops install/config: [Operations](/docs/03-operations/).

## Topics

{{< cards >}}
  {{< card link="runner-policy/" title="Runner Policy" subtitle="UI-managed concurrency, engines, cleanup" >}}
  {{< card link="docker-engine/" title="Docker Engine" subtitle="engine: docker + workspace mount" >}}
  {{< card link="ssh-multi-host/" title="SSH Multi-Host" subtitle="chain default · parallel opt-in · cancel-all" >}}
  {{< card link="sandbox-isolation-tiers/" title="Sandbox Isolation Tiers" subtitle="host → container → sandbox → vm" >}}
  {{< card link="sandbox-host-configuration/" title="Sandbox Host Configuration" subtitle="Manager-managed config + resource profiles" >}}
{{< /cards >}}
