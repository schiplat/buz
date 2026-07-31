# Buz

Lightweight Cross-Network Orchestration Platform.

**Single Binary, Unified Orchestration, Zero Trust.**

Current version: **1.0.1** (workspace `Cargo.toml` + `frontend/package.json`).

```bash
buz-manager --version
buz-runner --version
# Web UI: sidebar footer / login page; API: GET /api/v1/health
```

## Quick Start

```bash
# Build
cargo build --release

# Start Manager
./target/release/buz-manager

# Start Runner
export BUZ_REGISTER_TOKEN="your-token"
./target/release/buz-runner

# Frontend dev server
cd frontend && npm run dev

# Documentation
make docs-serve
# → http://127.0.0.1:3001/
```

## Architecture

Buz follows three core principles:

- **B (Binary)** — Standalone static binaries. No runtime dependencies.
- **U (Unified)** — Single Manager orchestrates runners across all network segments.
- **Z (Zero)** — Zero-trust security + zero operations burden.


## Tech Stack

| Layer | Technology |
|-------|-----------|
| Backend | Rust, axum, tokio, sqlx, SQLite, russh |
| Frontend | Vue 3, TypeScript, TailwindCSS, shadcn/ui, Xterm.js |
| Docs | Hugo + Hextra theme |

## Project Structure

```
buzproject/
├── crates/
│   ├── buz-core/      # Shared library
│   ├── buz-manager/   # Control plane
│   └── buz-runner/    # Execution agent
├── frontend/          # Vue 3 SPA
├── docs/              # Hugo documentation site
└── pipelines/         # Pipeline YAML definitions
```
