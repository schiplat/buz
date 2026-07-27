---
title: Project Structure
weight: 1
description: "Buz monorepo directory layout"
---

```
buzproject/
├── Cargo.toml              # Workspace root
├── conf/
│   ├── manager.toml          # Manager default config
│   └── runner.toml           # Runner default config
├── pipelines/              # Pipeline YAML definitions
├── crates/
│   ├── buz-core/           # Shared library
│   │   └── src/
│   │       ├── models/     # Data models (7 tables)
│   │       ├── protocol/   # Request/response DTOs, ApiResponse
│   │       ├── error.rs    # AppError + IntoResponse
│   │       └── auth.rs     # TokenManager, JWT, Claims
│   ├── buz-manager/        # Manager binary
│   │   └── src/
│   │       ├── main.rs     # axum bootstrap
│   │       ├── config.rs   # TOML config loader
│   │       ├── db/         # SQLite init + migrations
│   │       ├── api/        # Route handlers
│   │       ├── middleware/ # Auth middleware
│   │       └── services/   # Business logic layer
│   └── buz-runner/         # Runner binary
│       └── src/
│           ├── main.rs     # Register → heartbeat → poll loop
│           ├── config.rs
│           ├── client/     # HTTP communication
│           ├── engine/     # Execution engines (local, ssh)
│           ├── secret/     # Vault secret management
│           └── masker/     # Log masking (Aho-Corasick)
├── frontend/               # Vue 3 + TypeScript
│   └── src/
│       ├── router/         # Hash-mode routes
│       ├── store/          # Pinia stores
│       ├── api/            # Axios + interceptors
│       ├── hooks/          # Composable hooks
│       ├── components/     # Layout + business components
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

# Frontend dev server
cd frontend && npm run dev

# Docs
cd docs && hugo server --port 3001
```
