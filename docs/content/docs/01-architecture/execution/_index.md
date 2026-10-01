---
title: Execution
weight: 20
description: "How the Runner executes steps — Docker, SSH multi-host, runner policy"
---

## What this section is

**Runtime** behavior on the Runner: engines (`docker` / `ssh` / `shell`), multi-host fan-out, cancel, and UI-managed runner policy (concurrency, cleanup, host keys).

## Reading order

1. [Runner Policy](./runner-concurrent-jobs/) — concurrency, engines enabled, cleanup, SSH host-key policy
2. [Docker Engine](./docker-engine/) — `engine: docker`, `registry_secret`, workspace mount
3. [SSH Multi-Host](./ssh-multi-host/) — parallel/serial fan-out, cancel-all, log banners

Authoring fields live in [Pipeline Format](/docs/01-architecture/authoring/pipeline-format/).

## Topics

{{< cards >}}
  {{< card link="runner-concurrent-jobs/" title="Runner Policy" subtitle="UI-managed concurrency, engines, cleanup" >}}
  {{< card link="docker-engine/" title="Docker Engine" subtitle="engine: docker + registry_secret" >}}
  {{< card link="ssh-multi-host/" title="SSH Multi-Host" subtitle="chain default · parallel opt-in · cancel-all" >}}
{{< /cards >}}

Ops install/config: [Operations](/docs/03-operations/).
