# Buz

**YAML pipelines across firewalled networks — the Manager is the only
component that listens.** Runners connect outbound-only: machines behind
NATs, in other clouds, or on partner networks run the same pipeline with
zero inbound ports, no VPN, no port forwarding.

[Buz Docs](https://buz.docs.sconts.com/) ·
[Plugin Marketplace](https://plugins.buz.schiplat.com/) ·
[Releases](https://github.com/schiplat/buz/releases) ·
[Security Policy](SECURITY.md)

[English](README.md) | [简体中文](README.zh-CN.md)

![Buz overview](docs/static/image/buz_overview.png)

## What is Buz

Buz takes the proven halves of two platforms you already know and adds what
neither has. From **GitHub Actions**: declarative YAML workflows, `uses:`
reusable steps, GHA-style `${secrets.NAME}` secrets. From **Jenkins**: the
governance core — environments with approval gates, RBAC, audit logs. From
**Buz itself**: an outbound-only network model. A small control plane (the
**Manager**) is the only component that listens; a fleet of execution agents
(**Runners**) dial out to claim jobs and reach hosts spread across
firewalls, NATs, and network segments — a build box in one datacenter, a
deploy target behind a corporate proxy, an arm64 board in a lab rack. Every
role ships as a single static Rust binary, and a full Manager + Runner stack
comes up in one container in five minutes.

It exists for the environment where a full CI/CD platform or Kubernetes is
too much: you want declarative pipelines, secrets, and an audit trail, but
the machines are heterogeneous and the network is hostile.

It is also ready for how automation gets written next: **AI agents as
pipeline authors**. Buz's primitives are safe to generate against — plain
declarative YAML instead of scripted Groovy; `uses:` references locked to a
content hash at save time, so nothing upstream can rewrite a saved
pipeline; secrets that never auto-inject into the process environment;
approval gates that keep a human in the loop when an agent proposes a
production deploy; and complete job logs an agent can read back to
self-correct. See the full [comparison with Drone, GitHub Actions, and
Jenkins](https://buz.docs.sconts.com/docs/01-architecture/comparison/).

- **One binary per role.** `buz-manager` (control plane), `buz-runner`
  (execution agent), `buz-sandboxd` (optional host supervisor). Static
  Rust binaries — no runtime dependencies, no agent frameworks.
- **Pipelines are YAML.** Triggers (manual / webhook / cron), environments
  with approval gates, concurrency queues, retries, and per-step timeouts —
  declared, not configured in a UI.
- **Steps are shell, SSH, Docker, or plugins.** `uses:` references resolve
  against a plugin marketplace and are content-hash locked at save time.
- **Secrets are GHA-style.** `${secrets.NAME}` at the use site, encrypted at
  rest (AES-256-GCM), never auto-injected into the process environment.

## How it works

The Manager is the only component that listens. Runners live wherever the
work is and connect **outbound** to claim jobs over long-poll HTTP — so a
Runner behind NAT, in a different cloud, or on a partner network needs zero
inbound ports, port forwarding, or VPN.

Deployment targets are reached by the Runner over SSH with its own
credentials, or as containers on the Runner host itself:

```text
┌──────────────────┐        outbound HTTPS/WS        ┌──────────────────┐
│      Manager     │ ◄────────────────────────────── │      Runner      │
│  control plane   │   long-poll claim / log push    │  execution agent │
│  dashboard, API  │                                 │  shell/ssh/docker│
└──────────────────┘                                 └────────┬─────────┘
        │ audits, RBAC, approvals                              │ ssh / docker
        ▼                                                      ▼
  ┌─────────────┐                                      ┌─────────────┐
  │  Dashboard  │                                      │  Hosts      │
  └─────────────┘                                      └─────────────┘
```

![Buz pipeline steps](docs/static/image/buz_steps.png)

A few design choices worth knowing up front:

- **Resolve & lock.** `uses:` plugin references are resolved against a
  plugin index when the pipeline is saved; the resolved version and content
  hash are locked into the pipeline, so upstream template changes cannot
  alter an already-saved pipeline.
- **Environments are mandatory.** Every pipeline declares the environments
  it may target (`environments: [staging, prod]`); every run picks one
  explicitly. Optional approval gates can require a second approver per
  environment.
- **Isolation is tiered and honest.** Execution modes range from `host` (no
  sandbox) through `container` to `sandbox` (gVisor) and `vm`
  (Firecracker-compatible). The Runner reports what its host can deliver; a
  human picks the tier; unavailable tiers fail jobs instead of silently
  downgrading. Docker socket access remains host-root-equivalent — Buz does
  not pretend otherwise. The full trust model is in
  [SECURITY.md](SECURITY.md).
- **The plugin marketplace is reviewed.** Official `buz/*` templates get a
  human first review, then CI tracks every version against the reviewed
  content hash. The egress surface of each official template is allowlisted.

The full architecture — pipeline authoring, execution engines, sandbox
tiers, RBAC, audit logs, environments — is documented in the
[Buz Docs](https://buz.docs.sconts.com/).

## Quick start — the 5-minute demo

A single container runs the whole stack (Manager + co-located Runner) on
one box. Copy-paste friendly; nothing is written to any repository.

**Requirements:** Linux or Docker Desktop (macOS/Windows), `docker` +
`docker compose`, `curl`, `openssl`. The image is multi-arch
(`linux/amd64` + `linux/arm64`) — Intel and Apple Silicon Macs both work.

```bash
# 1. Stage a deploy directory and pull the compose template
mkdir -p /tmp/buz-demo && cd /tmp/buz-demo
curl -fsSLO https://raw.githubusercontent.com/schiplat/buz/master/deploy/standalone/docker-compose.yml

# 2. Demo secret (generated here, never committed) — then launch
echo "BUZ_JWT_SECRET=$(openssl rand -hex 32)" > .env && chmod 600 .env
docker compose up -d
until curl -sf http://localhost:8080/health > /dev/null; do sleep 1; done
echo "Manager is up"
```

Then in the browser:

1. Open **http://localhost:8080** — the first visit walks you through
   creating the admin account (`/setup`).
2. The image ships with `[standalone] auto_approve = ["standalone-runner"]`:
   the bundled Runner registers over loopback automatically and shows
   **online** under *Runners* — no manual registration.
3. Create a pipeline in the UI (one `echo` shell step), pick the
   `standalone-runner` label, trigger it, and watch it get claimed,
   executed, and succeed — with full log output in the Job view.
4. Optional: import
   [`example/pipelines/oss-config-demo.yml`](example/pipelines/oss-config-demo.yml)
   from this repo to see the plugin flow end to end
   (`buz/config-fetch` bundle download + `buz/env-render` rendering of
   `.env`). Enter the referenced secret values under *Secrets* first.

What this proves in five minutes: Manager/Runner pairing, the dashboard,
job scheduling and execution, the plugin marketplace, and secret encryption
at rest — the feature surface of a production deployment, on one box.

> **This image is for evaluation.** It enables `[standalone]` loopback
> registration and runs everything in one container — convenient for a lab,
> not a production topology. For real deployments use the split
> `buz-manager` / `buz-runner` packages ([Deployment
> guide](docs/content/docs/03-operations/deployment.md)).

Cleanup: `docker compose down`, then delete the demo directory.

## Development

Working in this repository requires a Rust toolchain (pinned to **1.99.1**
via `rust-toolchain.toml` — rustup honors it automatically), Node.js for the
dashboard, GNU `make`, and optionally Docker for container tests.

```bash
# Build everything (dashboard is embedded into buz-manager at compile time)
make build

# Run a local instance against the example scaffold
make run          # → http://localhost:8080 (first visit → /setup)

# Or the fast path during iteration (debug build)
make dev
```

### Tests

`scripts/test.sh` runs feature-grouped test targets so you do not pay for
the full workspace on every iteration:

```bash
scripts/test.sh list           # show groups (runner, sandbox, pipeline, auth, plugins, …)
scripts/test.sh sandbox        # one group
scripts/test.sh '^slot_scheduling_test$'   # regex over test targets
scripts/test.sh all            # CI-equivalent: fmt + clippy + cargo test --all + dashboard
```

`all` mirrors [.github/workflows/ci.yml](.github/workflows/ci.yml) exactly —
run it before pushing. Anything else is a development shortcut.

### Repository layout

```text
buz/
├── Cargo.toml                 # workspace: shared / manager / runner / plugin-cli
├── crates/
│   ├── shared/                # protocol, pipeline YAML schema, crypto, shared types
│   ├── manager/               # control plane → buz-manager
│   ├── runner/                # execution agent → buz-runner, buz-sandboxd
│   └── plugin-cli/            # plugin authoring tool → buz-plugin
├── dashboard/                 # Vue 3 SPA, embedded into buz-manager at build
├── docs/                      # Hugo + Hextra documentation site
├── example/                   # local run scaffold (conf/, pipelines/, data/)
├── deploy/                    # release config templates, standalone compose, nginx samples
├── scripts/                   # release packaging, test runner, ops helpers
├── docker/                    # Dockerfiles + compose overlays (incl. multi-arch standalone)
└── .github/workflows/         # CI, release, Docker image publishing
```

### Tech stack

| Layer | Technology |
|-------|------------|
| Backend | Rust, axum, tokio, sqlx, SQLite, russh |
| Frontend | Vue 3, TypeScript, TailwindCSS, shadcn/ui, Xterm.js |
| Docs | Hugo + Hextra theme |

### Contributing

- Branch from `master`, open a PR against `master`.
- Before pushing: `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --all`, and the dashboard
  `npm test` + `npm run build` — the same set CI runs. `scripts/test.sh all`
  is the one-command version.
- Docs-only commits skip the Rust CI via `paths-ignore` and are handled by
  the documentation deploy workflow.
- Plugin PRs live in [schiplat/buz-plugins](https://github.com/schiplat/buz-plugins) —
  every official template needs a human first review before it is
  ci-tracked.

## Security

**Please do not report vulnerabilities through public GitHub issues.** Use
[private vulnerability reporting](https://github.com/schiplat/buz/security/advisories/new)
— reports stay private, first response targets 7 days, and good-faith
research is covered by a safe harbor. Suspected malicious **step templates**
are reported the same way in
[buz-plugins](https://github.com/schiplat/buz-plugins/security/advisories/new).

The full policy — trust model, known limitations (no rate limiting, JWT
secret as root key, runner tenancy boundary, execution-tier honesty, and
more), the dependency-advisory policy, and the supported rotation procedure
(`buz-manager rotate-jwt-key`) — is documented in
[SECURITY.md](SECURITY.md).

Two points evaluators should know before exposing anything:

- The standalone demo image enables loopback self-registration and runs
  Manager + Runner in one box. It is a lab tool — do not expose it.
- Runners execute untrusted pipeline code under the Runner's OS user; jobs
  on one Runner are **not** isolated from each other. Use one Runner host
  per trust boundary.

## License

[Apache-2.0](LICENSE)
