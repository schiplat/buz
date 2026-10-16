---
title: Operations
weight: 4
description: "Configuration, deployment, and operations guides"
---

{{< cards >}}
  {{< card link="configuration/" title="Configuration" subtitle="manager.toml / runner.toml" >}}
  {{< card link="config-file-scopes/" title="Config File Scopes" subtitle="manager.toml / runner.toml parameter scopes" >}}
  {{< card link="deployment/" title="Deployment" subtitle="Install Manager and Runner" >}}
  {{< card link="standalone/" title="Standalone" subtitle="Single-image first run & upgrade (arm64)" >}}
  {{< card link="tls/" title="HTTPS" subtitle="Manager TLS, Runner client, reverse proxy, commands" >}}
  {{< card link="runner-restart/" title="Runner Restart" subtitle="Remotely restart a Runner from the UI" >}}
{{< /cards >}}

Local first run: [Quick Start](/docs/00-quick-start/). Production detail: [Deployment](/docs/03-operations/deployment/).

Runner strategy (concurrency, engines, cleanup) is configured in the UI — see [Runner Policy](/docs/01-architecture/execution/runner-concurrent-jobs/).

First admin is created at **`/setup`** on a fresh database (not from config).
