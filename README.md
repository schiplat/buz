# Buz

Lightweight Cross-Network Orchestration Platform.

**Single Binary, Unified Orchestration, Zero Trust.**

Current version: **1.5.6** (workspace `Cargo.toml` + `dashboard/package.json`).

```bash
buz-manager --version
buz-runner --version
# Web UI: sidebar footer / login page; API: GET /health
```

## Quick Start

```bash
# One-step: build dashboard + backend and start Manager
make run
# → http://localhost:8080  (first visit → /setup to create admin)

# Or step by step
make example        # create data/pipelines dirs (configs in example/conf/)
make build          # dashboard + release binaries (UI embedded in buz-manager)
cd example && BUZ_ALLOW_INSECURE_JWT=1 ../target/release/buz-manager

# Runner (register_token from Manager UI → Runners)
export BUZ_REGISTER_TOKEN="your-token"
./target/release/buz-runner -c example/conf/runner.toml

# Docs site (local)
make docs-serve
# → http://127.0.0.1:3001/  (or :1313 depending on Hugo)
```

**Lab / single-VM (arm64):** one-container image — see [Standalone](docs/content/docs/03-operations/standalone.md) (`buz-standalone`).

More: [docs Quick Start](docs/content/docs/00-quick-start/_index.md) · [Deployment](docs/content/docs/03-operations/deployment.md) · [Configuration](docs/content/docs/03-operations/configuration.md) (includes `[standalone]`).

## Architecture

Buz follows three core principles:

- **B (Binary)** — Standalone static binaries. No runtime dependencies.
- **U (Unified)** — Single Manager orchestrates runners across all network segments.
- **Z (Zero)** — Zero-trust security + zero operations burden.

Docs layout: **Quick Start → Architecture** (Authoring / Execution / Platform) → **Development** → **Operations**.

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Backend | Rust, axum, tokio, sqlx, SQLite, russh |
| Frontend | Vue 3, TypeScript, TailwindCSS, shadcn/ui, Xterm.js |
| Docs | Hugo + Hextra theme |

## Project Structure

```
buzproject/
├── Cargo.toml                 # Workspace (shared / manager / runner)
├── Makefile
├── crates/
│   ├── shared/                # Protocol, pipeline YAML schema, shared types
│   ├── manager/               # Control plane → buz-manager
│   └── runner/                # Execution agent → buz-runner
├── dashboard/                 # Vue 3 SPA (embedded into buz-manager at build)
├── docs/                      # Hugo + Hextra documentation site
├── example/                   # Local run scaffold
│   ├── conf/                  # manager.toml / runner.toml
│   └── pipelines/             # Sample pipeline YAML
├── deploy/                    # Packaged configs + ops scripts
│   ├── conf/                  # Docker / release config templates
│   ├── standalone/            # buz-standalone Compose
│   └── nginx/                 # Reverse-proxy examples
├── scripts/                   # Release packaging, e2e helpers
├── Dockerfile                 # buz-manager image
├── Dockerfile.runner          # buz-runner image
├── Dockerfile.standalone      # Manager + Runner (arm64 lab image)
├── docker-compose.yml
└── .github/workflows/         # CI, Docker, release
```
