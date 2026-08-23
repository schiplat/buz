---
title: Platform
weight: 30
description: "Control plane — RBAC, secrets ACL, API, admin audit"
---

## What this section is

Manager **control-plane** concerns: users/teams/RBAC, how secrets are stored and bound, HTTP API surface, and admin audit logs.

## Reading order

1. [Users, Teams & RBAC](./rbac/)
2. [Secrets Access (GHA-style)](./secrets-access/) — runtime mapping rules
3. [Secrets Access Control](./secrets-access-control/) — kind, team_shared, bindings
4. [GitHub Actions Secrets/Variables Sync](./github-actions-sync/) — remote push to GitHub repos
5. [API Reference](./api-reference/)
6. [Admin Audit Logs](./admin-audit-logs/)

Pipeline YAML fields: [Authoring → Pipeline Format](/docs/01-architecture/authoring/pipeline-format/).

## Topics

{{< cards >}}
  {{< card link="rbac/" title="Users, Teams & RBAC" subtitle="Roles, team scope, hosts" >}}
  {{< card link="secrets-access/" title="Secrets Access" subtitle="Allowlist → map → consume" >}}
  {{< card link="secrets-access-control/" title="Secrets Access Control" subtitle="kind, team_shared, bindings" >}}
  {{< card link="github-actions-sync/" title="GitHub Actions Sync" subtitle="Bind secrets/vars to repos, push to Actions" >}}
  {{< card link="api-reference/" title="API Reference" subtitle="HTTP endpoints" >}}
  {{< card link="admin-audit-logs/" title="Admin Audit Logs" subtitle="Mutation audit + redaction" >}}
{{< /cards >}}
