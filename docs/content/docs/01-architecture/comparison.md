---
title: "Comparison"
weight: 2
description: "Buz + buz-plugins vs Drone vs GitHub Actions vs Jenkins — architecture, network model, pipelines, plugin supply chain, secrets, governance, isolation, cost"
---

Buz borrows the proven halves of two platforms you already know — the
declarative authoring of **GitHub Actions** and the governance core of
**Jenkins** — and adds what neither has: an outbound-only network model that
reaches machines across firewalls. This page compares Buz (plus its
official plugin marketplace) with Drone, GitHub Actions, and Jenkins across
eight dimensions.

> Facts below reflect public documentation and this repository as of
> 2026-11-09; vendor claims were re-verified against official docs on that
> date. Vendor capabilities evolve — verify before making a decision. Buz-side
> claims link to the relevant pages of this site.

## At a glance

| | Buz + buz-plugins | Drone | GitHub Actions | Jenkins |
|---|---|---|---|---|
| Control plane | Manager: single static Rust binary, self-hosted | drone-server (Go), self-hosted | Hosted runners are SaaS-only; control plane self-hostable via GHES (bring your own runners) | Jenkins controller (JVM), self-hosted |
| Network model | Runners connect outbound-only; **zero inbound ports** | Runners poll the server outbound, but execution is confined to the runner host | Self-hosted runners poll github.com outbound | Agents typically need the controller to be reachable |
| Reach across firewalls/NAT | Core design goal | Partial — work runs only where runners (or attached clusters) are placed | Runner must sit inside the target network **and** reach github.com | Painful (DMZ / tunnel patterns) |
| Authoring | Declarative YAML, GHA-style `uses:` | YAML | YAML + expressions | Groovy (Jenkinsfile) |
| Governance | Built in: approval gates, RBAC, audit logs | Enterprise-tier features | Environments + required reviewers (paid for private repos) | Plugin-dependent |
| Plugin supply chain | Human-reviewed, content-hash locked, signed index, egress allowlists | Docker-image plugins, no review | Largest ecosystem, no review of marketplace actions | 2300+ plugins, community-maintained |
| Setup to first run | 5 minutes (single container, auto-registered Runner) | ~10 minutes | Minutes (needs a GitHub repo) | Hours to days |

## Architecture and deployment

| Dimension | Buz + buz-plugins | Drone | GitHub Actions | Jenkins |
|---|---|---|---|---|
| Control plane | `buz-manager`: single static Rust binary, self-hosted | `drone-server`, self-hosted Go binary or container | github.com SaaS (with hosted runners); GHES self-hosts the control plane but requires your own runner fleet | Jenkins controller on a JVM, self-hosted |
| Execution component | `buz-runner` (execution agent) + optional `buz-sandboxd` (host supervisor); measured idle RSS: Runner ≈ 25 MB, Manager ≈ 25–45 MB (macOS/arm64 release build, one registered runner, no jobs, 2026-11-08) | `drone-runner` drives Docker containers (experimental exec runner runs on the host shell) | Hosted ephemeral VMs, or self-hosted runners on any machine | Persistent JVM agents per host |
| Runtime dependencies | None — static binaries, no agent frameworks | Docker required on the runner host | None (hosted) / whatever self-hosting needs | JDK everywhere |
| Time to first pipeline | 5-minute standalone demo: one container, Runner auto-registers over loopback ([Quick Start](/docs/00-quick-start/)) | Tens of minutes, straightforward | Minutes, but requires a GitHub repo | Hours to days: plugin selection, controller tuning |
| CPU architectures | Official multi-arch images (linux/amd64 + linux/arm64) | Wherever Docker runs | Hosted: x64 and arm64 Linux; self-hosted: any | Very broad |

## Network model

This is where the platforms genuinely differ — everything else is degrees.

| Dimension | Buz | Drone | GitHub Actions | Jenkins |
|---|---|---|---|---|
| Connection direction | Runners connect **outbound-only** (long-poll claim, log push); the Manager is the only component that listens | Runners dial the server outbound over gRPC (docker / kubernetes / exec runner variants) | Self-hosted runners poll github.com outbound | Two common modes: "inbound" agents dial the controller over JNLP/WebSocket — the controller endpoint must be reachable from agent networks; "outbound" agents are driven by the controller over SSH — the controller must reach the agent |
| Inbound ports on execution machines | **Zero** — no port forwarding, no VPN ([How it works](https://github.com/schiplat/buz#how-it-works)) | Zero on the runner host | Zero on the runner host | Controller must expose ports; cross-segment setups often need holes or tunnels |
| Where work can execute | On the Runner host (containers) **or on arbitrary remote hosts over SSH** with Runner-held credentials | Only where you place a runner — Docker containers on the runner host, pods in an attached Kubernetes cluster (kube runner, experimental), or host shell (exec runner, experimental); never remote SSH targets | Only on the runner (hosted VM or self-hosted machine) | Only on hosts that have an agent installed — every segment needs its own agents |
| NAT / multi-cloud / partner networks | Designed for it: Runners live wherever the work is | Runners can sit in any private network (they dial out), but a single runner executes only on its own host or attached cluster — reaching machines in other segments means placing more runners there | The runner must sit inside the target network and be able to reach github.com over the internet | DMZ and tunnel topologies; operationally heavy |
| Reaching heterogeneous targets (bare metal, lab boards, legacy boxes) | First-class: SSH steps with Runner-managed credentials | Not first-class — no built-in SSH fan-out; would be DIY shell steps | Not supported (runner-host execution only) | Possible but requires an agent on every target class |

## Pipeline authoring and execution

| Dimension | Buz | Drone | GitHub Actions | Jenkins |
|---|---|---|---|---|
| Format | Declarative YAML ([Pipeline Format](/docs/01-architecture/authoring/pipeline-format/)) | Declarative YAML | YAML + expressions | Groovy (Jenkinsfile) |
| Learning curve | Low — a pipeline is a short YAML document | Low | Moderate | Steep — Groovy sandbox, script approval |
| Triggers | Manual / webhook / cron | Webhook / CRON / promotions | Richest set: push, PR, issues, dispatch, workflow_run, schedules, and more | Webhook / polling / cron, configured in UI or code |
| Concurrency, retries, timeouts | Built in: concurrency queues, retries, per-step timeouts — declared in YAML ([Concurrency Queue](/docs/01-architecture/authoring/job-concurrency-queue/)) | Runner-level concurrency limits | Concurrency groups, `timeout-minutes`; no native step retry | `retry` / `timeout` are bundled Pipeline steps; `Lockable Resources` is a plugin |
| Environments | **Mandatory**: pipelines declare target environments; every run picks one explicitly ([Environments](/docs/01-architecture/platform/environments/)) | No first-class concept | Environments (optional) | No native concept |
| Step types | Shell, SSH, Docker, or plugins (`uses:`) | Container steps | run/shell on the runner, container jobs, composite actions | Anything Groovy can do, on any agent |
| Parameterization | Parameter scopes ([Parameter Scopes](/docs/01-architecture/authoring/pipeline-parameter-scopes/)) | Pipeline params | Workflow inputs, matrix | Parameters (UI or code) |

## Plugin ecosystem and supply chain

| Dimension | Buz + buz-plugins | Drone | GitHub Actions | Jenkins |
|---|---|---|---|---|
| Ecosystem size | Early-stage official catalog (notifications, deploy helpers, health checks) — [plugins.buz.schiplat.com](https://plugins.buz.schiplat.com) | Moderate: community Docker-image plugins | Largest: tens of thousands of marketplace actions | Large: 2300+ plugins (official index) |
| Human review before publishing | ✅ Official `buz/*` templates get a human first review; CI tracks every version against the reviewed content hash ([Plugin Tooling](/docs/01-architecture/authoring/plugin-tooling/)) | ❌ | ❌ — GitHub does not review marketplace actions (pin by SHA is a DIY mitigation) | ❌ — community-maintained |
| Immutable resolution | ✅ `uses:` references are resolved and locked to a content hash **at save time**; upstream changes cannot alter a saved pipeline | Mutable image tags | ❌ mutable tags unless manually pinned to a commit SHA | Explicit version pinning |
| Verifiable distribution | ✅ Signed plugin index (`index.json.sig`), signed review ledger, drift sentinel — a Manager under a trust anchor refuses an unverifiable index | ❌ | ❌ (artifact attestations exist for build outputs, not for actions themselves) | ❌ |
| Egress control | ✅ Each official template ships an allowlisted egress surface | ❌ | ❌ — self-managed network policies | ❌ |

## Secrets

| Dimension | Buz | Drone | GitHub Actions | Jenkins |
|---|---|---|---|---|
| Built-in storage | ✅ AES-256-GCM encryption at rest ([Secrets & Access](/docs/01-architecture/platform/secrets-access-control/)) | ✅ Basic secrets | ✅ org / repo / environment tiers | ✅ Credentials plugin |
| Injection model | `${secrets.NAME}` at the use site; **never auto-injected** into the process environment | Environment injection | Explicit references, masked in logs | `withCredentials` wrapping |
| GitHub Actions interop | ✅ Bind secrets/variables to GitHub repos and push them to Actions with one click ([GHA Sync](/docs/01-architecture/platform/github-actions-sync/)) | — | — | — |

## Governance and compliance

| Dimension | Buz | Drone | GitHub Actions | Jenkins |
|---|---|---|---|---|
| Approval gates | ✅ Built-in environment gates, optional second approver, [switchable](/docs/01-architecture/platform/ops-approval-switch/) | No environment-style gates in OSS; promotions, RBAC and audit sit in the source-available Enterprise edition | ✅ Environments with required reviewers — free in public repos, paid plans for private repos | ⚠️ No environment-scoped gate model; the bundled Pipeline `input` step can pause a job for manual approval |
| RBAC | ✅ Built-in ([RBAC](/docs/01-architecture/platform/rbac/)) | SCM-derived permissions; finer control in Drone Enterprise | GitHub's repo/org/team permission system | Plugins (Role-based Authorization, Matrix) |
| Audit logs | ✅ Built-in admin audit logs ([Audit Logs](/docs/01-architecture/platform/admin-audit-logs/)) | Drone Enterprise | Organization audit log | Plugins (Audit Trail) |

## Execution isolation

| Dimension | Buz | Drone | GitHub Actions | Jenkins |
|---|---|---|---|---|
| Isolation tiers | `host` → `container` → `sandbox` (gVisor) → `vm` (Firecracker-compatible); the Runner reports what its host can deliver, a human picks the tier, and an unavailable tier **fails the job instead of silently downgrading** ([Principles](/docs/01-architecture/principles/)) | Container-level (Docker runner; kube runner as pods in an attached cluster); exec runner runs without isolation (experimental) | Hosted: one-shot VMs; self-hosted: your responsibility | Persistent agents (not one-shot by default); containerization optional |
| Honest risk disclosure | ✅ Documents that Docker socket access is host-root-equivalent ([SECURITY.md](https://github.com/schiplat/buz/blob/master/SECURITY.md)) | Rarely discussed | — | Rarely discussed |

## Cost and licensing

| Dimension | Buz | Drone | GitHub Actions | Jenkins |
|---|---|---|---|---|
| License | Apache-2.0 | Apache-2.0 OSS core; Enterprise edition (source-available, Polyform) gates governance features | SaaS subscription + per-minute billing | MIT |
| Private-repo cost | None (self-hosted, no per-minute metering) | None | Paid minutes; exhausted org quotas push teams to create new orgs and re-migrate repos | No license cost; high operational headcount cost |
| Operational footprint | Two small static binaries + optional sandbox supervisor | Server + runner, Docker required | None (SaaS) | JVM controller(s), agent fleet, plugin upgrade treadmill |

## When to choose which

- **GitHub Actions** — your code already lives on GitHub, a SaaS control plane is acceptable, and ecosystem breadth outweighs supply-chain review gaps.
- **Jenkins** — you need deep Groovy-level customization or carry a large legacy of in-house integrations and expertise.
- **Drone** — container-only workloads, SCM-agnostic self-hosting (Gitea and friends), minimal governance needs.
- **Buz** — execution targets are scattered across firewalls, NATs, and network segments; you want governance (approvals, RBAC, audit) and a reviewed plugin supply chain built in rather than assembled from plugins; or you want a platform whose primitives are safe for AI-agent-authored pipelines — declarative YAML, save-time content-hash locking, non-injected secrets, approval gates on production deploys.
