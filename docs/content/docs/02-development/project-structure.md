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
├── docker/
│   ├── Dockerfile          # Manager image → /app/buz/
│   ├── Dockerfile.runner   # Runner image → /app/buz/
│   ├── docker-compose.yml  # Mounts deploy/conf → /app/buz/conf (build context: repo root)
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

## Component boundaries

Buz is one of two components. The step-template plugins — template content,
tool images, the static marketplace site, and their review governance — live
in the separate `buz-plugins` repository. Buz stays complete and testable
with zero plugins installed: resolution reads the local plugin index, and no
plugin content ships inside the binaries.

| Belongs in buz | Belongs in buz-plugins |
|----------------|------------------------|
| Execution semantics, engines, and the runtime interface ([Plugin Contract](/docs/01-architecture/authoring/plugin-contract/)) | Template content and per-plugin documentation |
| Spec schema + `plugin-validate` (alias `step-validate`) + the local plugin-index API | Tool images, static marketplace site, publishing pipelines |
| Review-gate mechanics (`require_reviewed`, `reviewed_manifest`) | Review records and the egress baseline as governance data |

Rule of thumb: *if the plugins repository disappeared tomorrow, would this
still be complete and testable?* Yes → it belongs in buz; no → plugins.
Buz must never embed template content, marketplace URLs beyond the
configurable `marketplace_url`, or publishing automation.

## API specification (OpenAPI)

The Manager serves its OpenAPI document at `/api/v1/openapi.json`, generated
from code by [utoipa](https://github.com/juhaku/utoipa) — it is never
hand-edited. Convention: when you touch a handler, annotate it with
`#[utoipa::path]` and register it in `crates/manager/src/api/openapi.rs`;
coverage grows incrementally with the code (`crates/manager/src/api/version.rs`
is the reference example). Generated specs are the foundation for
generated SDKs — no hand-written clients.

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
