---
title: Authoring
weight: 10
description: "Write pipelines — YAML format, expressions, config, triggers, notify, concurrency"
---

## What this section is

Everything about **authoring** a pipeline: file authority, field reference, secrets/vars syntax, config deliver/render, notifications, git webhooks, and job concurrency groups.

## Reading order

1. [YAML-First Authoring](./yaml-first-authoring/) — who owns the YAML (file/Git vs DB)
2. [Pipeline Format](./pipeline-format/) — **parameter reference** (all YAML fields)
3. [Secrets & Variables Expressions](./secrets-vars-expressions/) — cheat sheet
4. [Secrets Access (GHA-style)](../platform/secrets-access/) — allowlist → map → consume
5. [Config Block](./config-block/) + [Env Render](./env-render/) — when you ship `.env` / OSS bundles
6. [Pipeline Notify](./pipeline-notify/) / [Git Webhook Triggers](./git-webhook-triggers/) / [Job Concurrency Queue](./job-concurrency-queue/) — as needed

## Topics

{{< cards >}}
  {{< card link="yaml-first-authoring/" title="YAML-First Authoring" subtitle="File/Git authority; UI writes scan dir" >}}
  {{< card link="pipeline-format/" title="Pipeline Format" subtitle="YAML field reference + registration" >}}
  {{< card link="secrets-vars-expressions/" title="Secrets & Variables" subtitle="Expression cheat sheet" >}}
  {{< card link="config-block/" title="Config Block DSL v2" subtitle="OSS deliver + render-only" >}}
  {{< card link="env-render/" title="Env Render" subtitle="ValueMap, BUZ_ENV_CHANGED" >}}
  {{< card link="pipeline-notify/" title="Pipeline Notify" subtitle="Feishu end-of-build cards" >}}
  {{< card link="git-webhook-triggers/" title="Git Webhook Triggers" subtitle="GitHub/Gitee → INPUT_GIT_*" >}}
  {{< card link="job-concurrency-queue/" title="Job Concurrency Queue" subtitle="Pipeline-level queue / discard" >}}
{{< /cards >}}

Upstream: [Architecture](/docs/01-architecture/). Downstream: [Execution](/docs/01-architecture/execution/) (how steps run on the Runner).
