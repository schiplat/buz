---
title: Build from source
weight: 10
description: "Build, run and package Buz from source — the contributor workflow"
---

Building from source is the **contributor** workflow. Evaluators should use
the [standalone demo](/docs/00-quick-start/) or pull [release
packages](/docs/03-operations/deployment/#binary-deployment) instead.

## Prerequisites

- **Rust 1.99.1** — pinned via `rust-toolchain.toml`; rustup honors it
  automatically (`rustup` installed → `cd buz` is enough)
- **Node.js** — for the dashboard build
- **GNU make**, optionally **Docker** for container tests

## Build

```bash
# Dashboard first — it is embedded into buz-manager at compile time
cd dashboard && npm ci && npm run build && cd ..

# Backend + frontend (release binaries in target/release/)
make build          # = build-dashboard + build-backend
```

## Run locally

```bash
make example        # create example/conf + sample pipelines (configs committed)
make run            # build + start Manager  → http://localhost:8080 (first visit → /setup)

# Fast iteration without the release optimization:
make dev            # dashboard build + debug manager
```

Split processes for a two-machine or two-terminal lab:

```bash
make run            # terminal 1: Manager
make run-runner     # terminal 2: Runner (set register_token from UI → Runners first)
```

## Package release archives

```bash
make package        # host-native archive → dist/release/buz-<ver>-<os>-<arch>.tar.gz
```

The archive contains `bin/buz-manager` + `bin/buz-runner` (dashboard
embedded), `conf/` templates and a README — the same layout the CI release
publishes. CI builds the full platform matrix (linux-x86_64, linux-aarch64,
macos-aarch64) on `v*` tags.

## Tests

```bash
make ci             # CI-equivalent gate: fmt + clippy + cargo test --all + dashboard
scripts/test.sh list            # feature groups
scripts/test.sh sandbox         # one group — fast iteration
scripts/test.sh all             # same as make ci
```

`scripts/test.sh <group>` skips unrelated crates; `all` is the full gate and
must be green before pushing.


## End-to-end smoke on a VM

Minimal path after a source build, validating Manager + Runner + SSH deploy
on one host (e.g. a Linux VM with `make build` artifacts copied over):

1. Start Manager + Runner (`example/conf/*.toml`). Login with the configured
   admin.
2. Confirm **Access -> Teams** has `default` (created at bootstrap).
3. **Hosts**: register `dev-vm-1` with SSH key / password / agent.
4. Import `example/pipelines/` with
   `buz-manager pipeline import --dir example/pipelines` (add `--dry-run` to
   validate only).
5. Pipelines list should show the YAML pipelines under team `default`.
6. Trigger **SSH Verify**; open Job Detail - steps + live logs stream.
7. Runner labels in Manager UI must include `linux-x64` (match YAML
   `runner.labels`).

YAML team assignment:

```yaml
team: default   # or another existing team slug; omit -> default
```
