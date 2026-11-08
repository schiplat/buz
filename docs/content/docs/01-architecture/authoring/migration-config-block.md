---
title: "Migrating from the kernel `config:` block"
weight: 110
description: "The kernel config: block was removed — render env files with the buz/env-render plugin"
aliases:
  - /docs/01-architecture/authoring/env-render/
  - /docs/01-architecture/authoring/config-block/
---

The kernel `config:` block was **removed**. Render env files with the
[`buz/env-render`](https://plugins.buz.schiplat.com) plugin step.

**Recommended form — `values:` map:**

```yaml
- name: Render env
  uses: buz/env-render@1.1.0
  engine: ssh
  host: $deploy_host
  with:
    dest_dir: /opt/app
    template: env.example
    output: .env
    mode: "0600"
    on_missing: fail
    values: |
      POSTGRES_PASSWORD=${secrets.POSTGRES_PASSWORD}
```

**Alternative form — `values_prefix:` env transit:** secret values transit the
call site's `env:` mapping (masked in logs); the plugin's inputs never carry
raw secret values:

```yaml
- name: Render env
  uses: buz/env-render@1.1.0
  engine: ssh
  host: $deploy_host
  env:
    BUZV_POSTGRES_PASSWORD: ${secrets.POSTGRES_PASSWORD}
  with:
    dest_dir: /opt/app
    template: env.example
    output: .env
    mode: "0600"
    on_missing: fail
    values_prefix: BUZV_
```

`buz-manager scan-deprecated` lists pipelines still using the removed block.
Syntax details: [Secrets & Variables](./secrets-variables.md).
