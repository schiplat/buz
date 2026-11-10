---
title: CLI Reference
weight: 60
description: operator subcommands for buz-manager and buz-sandboxd — scan, rotate, plan, tokens, doctor
---

`buz-manager` ships operator subcommands alongside the server. All of them
work offline against the local database/config unless noted.

## scan-deprecated

Read-only pre-upgrade report: lists pipelines, secret rows and YAML files on
disk that still carry kernel surfaces removed in the 2.x retirement
(`registry_secret`, `config.source` / `config.key_path`, secret kinds
`registry` / `aliyun_oss_auth`).

```bash
buz-manager scan-deprecated
```

Run it **before upgrading an old instance** — it tells you exactly what to
migrate and how. Nothing is modified.

## rotate-jwt-key

Rotates the JWT secret and re-encrypts every stored value in place. See
[Key Rotation](./key-rotation.md) for the full procedure.

```bash
buz-manager rotate-jwt-key            # plan only — validates, zero writes
buz-manager rotate-jwt-key --apply    # snapshot + re-encrypt + verify
```

Stop the Manager service first. The current secret is resolved from the
same config chain the service uses (`-c` / `BUZ_JWT_SECRET`).

## gen-runner-token

Creates a runner record locally and prints its one-shot register token —
for automating runner provisioning without the web UI.

```bash
buz-manager gen-runner-token lab-132 --labels linux-arm64 \
  --token "$ADMIN_JWT"
```

## pipeline plan

Resolve a pipeline's `uses:` steps against a plugin index and print a
non-executing plan: resolved versions, content hashes, input mapping.

```bash
buz-manager pipeline plan my-pipeline.yml \
  --plugin-index ./plugins --offline
```

`not found` fails the command; registry reachability issues report as
`unverified` (or are skipped with `--offline`). Provenance is never
executed or revealed.

## plugin-validate

Validate plugin spec YAML files against the marketplace schema — the same
checker the buz-plugins CI runs.

```bash
buz-manager plugin-validate plugins/        # directory or single file
```

## completions

Print a shell completion script (generated from the same CLI definition):

```bash
buz-manager completions zsh > ~/.zfunc/_buz-manager
```

## buz-sandboxd --doctor

Read-only host readiness report for the sandboxed tiers. Needs no Manager,
token, or applied policy, so it works on a host that has not been enrolled yet.
Full procedure: [Sandbox Host Installation](/docs/03-operations/sandbox-host/).

```bash
buz-sandboxd --doctor                      # human table
buz-sandboxd --doctor --json               # machine-readable
buz-sandboxd --doctor --probe-image repo@sha256:… --probe-runtime runsc
```

Exit codes: `0` at least one tier's prerequisites are satisfied, `2` none,
`1` a requested probe failed. `--probe-image` must be digest-pinned and runs
the same create → start → exec → remove sequence as the Manager probe.

`buz-sandboxd --healthcheck` checks a running supervisor's socket (used by the
Compose healthcheck and systemd).
