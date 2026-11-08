---
title: Quick Start
weight: 1
description: "Run the full Buz stack in one container — 5-minute standalone demo"
---

Run the whole stack — **Manager + co-located Runner** — in one container, and
trigger your first job. Copy-paste friendly; nothing is written to any
repository.

> Requirements: Linux or Docker Desktop (macOS/Windows), `docker` +
> `docker compose`, `curl`, `openssl`. The image is multi-arch
> (`linux/amd64` + `linux/arm64`) — Intel and Apple Silicon Macs both work.

## 1. Launch the stack

```bash
mkdir -p /tmp/buz-demo && cd /tmp/buz-demo
curl -fsSLO https://raw.githubusercontent.com/schiplat/buz/master/deploy/standalone/docker-compose.yml
echo "BUZ_JWT_SECRET=$(openssl rand -hex 32)" > .env && chmod 600 .env
docker compose up -d
until curl -sf http://localhost:8080/health > /dev/null; do sleep 1; done
echo "Manager is up"
```

## 2. Create the admin account

Open **http://localhost:8080** — the first visit walks you through creating
the admin account (`/setup`).

## 3. The bundled Runner registers itself

The image ships with `[standalone] auto_approve = ["standalone-runner"]`:
the co-located Runner registers over loopback automatically and shows
**online** under *Runners* — no manual registration.

## 4. Run your first job

Create a pipeline in the UI (one `echo` shell step), pick the
`standalone-runner` label, trigger it, and watch it get claimed, executed,
and succeed — with full log output in the Job view.

## 5. Optional: the plugin render demo

Import
[`example/pipelines/oss-config-demo.yml`](https://github.com/schiplat/buz/blob/master/example/pipelines/oss-config-demo.yml)
from this repository to see plugins end to end (`buz/config-fetch` bundle
download + `buz/env-render` rendering of `.env`). Enter the referenced
secret values under *Secrets* first.

## Cleanup

`docker compose down`, then delete the demo directory.

## Where to next

| You want to… | Go to |
|---|---|
| Deploy for production (containers or release binaries) | [Deployment](/docs/03-operations/deployment/) |
| Build and run from source | [Development — Build from source](/docs/02-development/build-from-source/) |
| Author pipelines | [Pipeline Format](/docs/01-architecture/authoring/pipeline-format/) |
| Understand the design | [Core Concepts](/docs/00-quick-start/core-concepts/) |

> **This image is for evaluation.** It enables `[standalone]` loopback
> registration and runs everything in one container — convenient for a lab,
> not a production topology.
