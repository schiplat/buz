---
title: Operations
weight: 3
description: "Deployment, configuration, and operations guides"
---

{{< cards >}}
  {{< card link="deployment/" title="Deployment" subtitle="Install Manager and Runner" >}}
  {{< card link="standalone/" title="Standalone" subtitle="Single-image first run & upgrade (arm64)" >}}
  {{< card link="configuration/" title="Configuration" subtitle="manager.toml / runner.toml" >}}
  {{< card link="tls/" title="HTTPS" subtitle="Manager TLS, Runner client, reverse proxy, commands" >}}
{{< /cards >}}

Runner strategy (concurrency, engines, cleanup) is configured in the UI — see [Runner Policy](/docs/01-architecture/runner-concurrent-jobs/).

First admin is created at **`/setup`** on a fresh database (not from config).
