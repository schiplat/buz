---
title: Platform
weight: 1
description: "Control plane — RBAC, environments & approvals, secrets ACL, API, admin audit"
---

## Who should read this

You administer the **Manager control plane**: who can use what (RBAC, secret access), what gets approved (deploy gates, ops change approvals), and what gets recorded (audit).

## Reading order

1. [Users, Teams & RBAC](./rbac/)
2. [Secrets Access Control](./secrets-access-control/) — kind, team_shared, bindings
3. [Environments](./environments/) — Variable / Secret overlays at Trigger / claim
4. [Environment Approval Gates](./environment-approvals/) — protected env Trigger ACL + approvals
5. [Ops Approval Switch](./ops-approval-switch/) — two-person approval for host-config and grant changes
6. [GitHub Actions Secrets/Variables Sync](./github-actions-sync/) — push values to GitHub repos
7. [Admin Audit Logs](./admin-audit-logs/)
8. [API Reference](./api-reference/)

Pipeline YAML fields: [Authoring → Pipeline Format](/docs/01-architecture/authoring/pipeline-format/). Usage-side secret syntax: [Secrets & Variables](/docs/01-architecture/authoring/secrets-variables/).

## Topics

{{< cards >}}
  {{< card link="rbac/" title="Users, Teams & RBAC" subtitle="Roles, team scope, hosts" >}}
  {{< card link="secrets-access-control/" title="Secrets Access Control" subtitle="kind, team_shared, bindings" >}}
  {{< card link="environments/" title="Environments" subtitle="Staging/prod overlays for Variable & Secret defaults" >}}
  {{< card link="environment-approvals/" title="Environment Approval Gates" subtitle="Protected env Trigger ACL + append-only records" >}}
  {{< card link="ops-approval-switch/" title="Ops Approval Switch" subtitle="Two-person approval for host-config changes" >}}
  {{< card link="github-actions-sync/" title="GitHub Actions Sync" subtitle="Bind secrets/vars to repos, push to Actions" >}}
  {{< card link="admin-audit-logs/" title="Admin Audit Logs" subtitle="Mutation audit + redaction" >}}
  {{< card link="api-reference/" title="API Reference" subtitle="HTTP endpoints" >}}
{{< /cards >}}
