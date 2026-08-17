---
title: Quick Start
weight: 1
description: "Build and run Buz locally in minutes"
---

Get Manager (and optionally Runner) running on your machine. For production layouts, packages, Docker, and upgrades, continue to **[Deployment](/docs/03-operations/deployment/)**.

## One-step local run

```bash
# Build dashboard + backend and start Manager
make run
# → http://localhost:8080  (first visit → /setup to create admin)
```

## Step by step

```bash
make example        # create example/conf + sample pipelines
make build          # dashboard + release binaries
cd example && ../target/release/buz-manager
```

Open **http://localhost:8080** → `/setup` creates the first admin (not from config).

### Start a Runner

```bash
# register_token from Manager UI → Runners (or example/conf)
export BUZ_REGISTER_TOKEN="your-token"
./target/release/buz-runner -c example/conf/runner.toml
```

## Lab / single-VM (arm64)

Prefer the one-container image — see **[Standalone](/docs/03-operations/standalone/)** (`buz-standalone`).

## Next

| Guide | When |
|-------|------|
| [Deployment](/docs/03-operations/deployment/) | Release packages, split Manager + Runner, Docker, production |
| [Configuration](/docs/03-operations/configuration/) | `manager.toml` / `runner.toml` reference |
| [Pipeline Format](/docs/01-architecture/authoring/pipeline-format/) | Author your first YAML pipeline |
