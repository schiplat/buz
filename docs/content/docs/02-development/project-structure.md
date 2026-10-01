---
title: Project Structure
weight: 1
description: "Buz monorepo directory layout"
---

```
buz/
├── Cargo.toml              # Workspace root
├── conf/
│   ├── manager.toml        # Manager default config
│   └── runner.toml         # Runner default config
├── pipelines/              # Pipeline YAML definitions
├── crates/
│   ├── shared/             # Shared library
│   │   └── src/
│   │       ├── models/     # Data models
│   │       ├── protocol/   # Request/response DTOs, ApiResponse
│   │       ├── error.rs    # AppError + IntoResponse
│   │       └── auth.rs     # TokenManager, JWT, Claims
│   ├── manager/            # Manager binary (control plane)
│   │   └── src/
│   │       ├── main.rs     # axum bootstrap
│   │       ├── config.rs   # TOML config loader
│   │       ├── db/         # SQLite init + migrations
│   │       ├── api/        # Route handlers
│   │       ├── middleware/ # Auth middleware
│   │       └── services/   # Business logic layer
│   └── runner/             # Runner binary (execution agent)
│       └── src/
│           ├── main.rs     # Register → heartbeat → poll loop
│           ├── config.rs
│           ├── client/     # HTTP communication
│           ├── engine/     # Execution engines (shell, ssh, docker)
│           └── masker/     # Log masking (Aho-Corasick)
├── dashboard/              # Vue 3 + TypeScript (built into buz-manager via rust-embed)
├── Dockerfile              # Manager image → /app/buz/
├── Dockerfile.runner       # Runner image → /app/buz/
├── docker-compose.yml      # Mounts deploy/conf → /app/buz/conf
├── deploy/                 # Config templates (Docker + tar.gz) + README
│   └── conf/
│   └── src/
│       ├── router/         # History-mode routes (vue-router)
│       ├── stores/         # Pinia stores (auth, ui)
│       ├── api/            # Axios client + resource APIs
│       ├── utils/          # Navigation, time, WebSocket helpers
│       ├── components/     # Layout + business components
│       ├── layouts/        # AppShell layout
│       ├── pages/          # Route pages
│       └── types/          # TypeScript type definitions
└── docs/                   # Hugo documentation site
```

## Getting Started

```bash
# Backend
cargo build

# Run Center
./target/debug/buz-manager

# Run Runner (in another terminal)
./target/debug/buz-runner

# Dashboard dev server
cd dashboard && npm run dev

# Docs
cd docs && hugo server --port 3001
```
