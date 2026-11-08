---
title: Execution
weight: 20
description: "How the Runner executes steps — Docker, SSH multi-host, sandbox host configuration, runner policy"
---

## What this section is

**Runtime** behavior on the Runner: engines (`docker` / `ssh` / `shell`), multi-host fan-out, cancel, sandbox host configuration, and UI-managed runner policy (concurrency, cleanup, host keys).

## Reading order

1. [Runner Policy](./runner-policy/) — concurrency, engines enabled, cleanup, SSH host-key policy
2. [Docker Engine](./docker-engine/) — `engine: docker`, workspace mount, private-registry login via `buz/docker-login`
3. [SSH Multi-Host](./ssh-multi-host/) — parallel/serial fan-out, cancel-all, log banners
4. [Sandbox Host Configuration & Resource Profiles](./sandbox-host-configuration/) — design record: Manager-managed config, named profiles, quota-aware scheduling

Authoring fields live in [Pipeline Format](/docs/01-architecture/authoring/pipeline-format/).

## Topics

{{< cards >}}
  {{< card link="runner-policy/" title="Runner Policy" subtitle="UI-managed concurrency, engines, cleanup" >}}
  {{< card link="docker-engine/" title="Docker Engine" subtitle="engine: docker + workspace mount" >}}
  {{< card link="ssh-multi-host/" title="SSH Multi-Host" subtitle="chain default · parallel opt-in · cancel-all" >}}
  {{< card link="sandbox-host-configuration/" title="Sandbox Host Configuration" subtitle="Design record: Manager-managed config + resource profiles" >}}
{{< /cards >}}

Ops install/config: [Operations](/docs/03-operations/).
