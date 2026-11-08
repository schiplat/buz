---
title: Authoring
weight: 1
description: "Write pipelines — YAML format, parameters, secrets & variables, plugins, triggers, notify, concurrency"
---

## Who should read this

You are **writing or reviewing pipeline YAML**. Runtime behavior on the Runner lives in [Execution](../execution/); control-plane governance lives in [Platform](../platform/).

## Reading order

1. [YAML-First Authoring](./yaml-first-authoring/) — who owns the YAML (file/Git vs DB)
2. [Pipeline Format](./pipeline-format/) — **parameter reference** (all YAML fields)
3. [Pipeline Parameter Scopes](./pipeline-parameter-scopes/) — which key belongs to which scope
4. [Secrets & Variables](./secrets-variables/) — allowlist flow + every expression, one page
5. [Plugin Contract](./plugin-contract/) — the versioned interface `uses:` resolves against
6. [Plugin Tooling](./plugin-tooling/) — `buz-plugin` CLI, responsibility boundaries
7. [Git Webhook Triggers](./git-webhook-triggers/) — GitHub/Gitee push → INPUT_GIT_*
8. [Pipeline Input Validation](./pipeline-input-validation/) — declarative input constraints
9. [Job Concurrency & Queue](./job-concurrency-queue/) — pipeline-level queue / discard
10. [Pipeline Notify](./pipeline-notify/) — end-of-build notifications
11. [Migrating from the kernel `config:` block](./migration-config-block/) — legacy render/delivery

## Topics

{{< cards >}}
  {{< card link="yaml-first-authoring/" title="YAML-First Authoring" subtitle="File/Git authority; UI writes scan dir" >}}
  {{< card link="pipeline-format/" title="Pipeline Format" subtitle="YAML field reference + registration" >}}
  {{< card link="pipeline-parameter-scopes/" title="Parameter Scopes" subtitle="Which key belongs to which scope" >}}
  {{< card link="secrets-variables/" title="Secrets & Variables" subtitle="Allowlist flow + expression cheat sheet" >}}
  {{< card link="plugin-contract/" title="Plugin Contract" subtitle="The `uses:` interface, versioned" >}}
  {{< card link="plugin-tooling/" title="Plugin Tooling" subtitle="buz-plugin CLI · boundaries" >}}
  {{< card link="git-webhook-triggers/" title="Git Webhook Triggers" subtitle="GitHub/Gitee → INPUT_GIT_*" >}}
  {{< card link="pipeline-input-validation/" title="Input Validation" subtitle="Declarative type/pattern/options" >}}
  {{< card link="job-concurrency-queue/" title="Job Concurrency & Queue" subtitle="Pipeline-level queue / discard" >}}
  {{< card link="pipeline-notify/" title="Pipeline Notify" subtitle="Feishu end-of-build cards" >}}
  {{< card link="migration-config-block/" title="Migrating `config:`" subtitle="Kernel block removed → buz/env-render" >}}
{{< /cards >}}

Upstream: [Architecture](/docs/01-architecture/). Downstream: [Execution](../execution/) (how steps run on the Runner).
