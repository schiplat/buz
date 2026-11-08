---
title: 'Config Block (render-only) — removed'
weight: 55
description: 'The kernel config: block was removed — render env files with the buz/env-render plugin'
aliases:
  - /docs/01-architecture/authoring/config-block/
---

The kernel `config:` block was **removed**. Render env files with the
[`buz/env-render`](https://plugins.buz.schiplat.com) plugin step:

```yaml
- name: Render env
  uses: buz/env-render@1.1.0
  engine: ssh
  host: $deploy_host
  env:
    BUZV_POSTGRES_PASSWORD: ${{secrets.POSTGRES_PASSWORD}}
  with:
    dest_dir: /opt/app
    template: env.example
    output: .env
    mode: "0600"
    on_missing: fail
    values_prefix: BUZV_
```

Secret values transit the call site's `env:` mapping (masked in logs) — the
plugin's inputs never carry raw secret values. `buz-manager
scan-deprecated` lists pipelines still using the removed block.
