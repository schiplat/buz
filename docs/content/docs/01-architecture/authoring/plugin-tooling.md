---
title: Plugin Tooling
weight: 8
description: "Responsibility boundaries between the plugin contract, the buz-plugin CLI, the Manager, and the plugins repository"
---

# Plugin Tooling & Responsibility Boundaries

The plugin ecosystem splits across four actors. Each has exactly one sentence
of responsibility; everything else in this page derives from that split.

| Actor | Responsibility | Owns | Explicitly not |
|-------|----------------|------|----------------|
| `shared` crate | The contract: what is a legal plugin | `step_spec` types + `validate_step_spec` (single authority) | Any CLI or I/O |
| `buz-plugin` CLI | What authors, publishers, and mirror operators do | `init`, `validate`, `index`, `mirror check` | Runtime enforcement — it is feedback, not a gate |
| `buz-manager` | Enforcing validation at the moment it consumes a template | Save-time resolution & locking, enable-time verification (`verify_entry`), the review gate, mirror failover | Authoring tools — `step-validate` retires once `buz-plugin` takes over |
| plugins repository | Content and governance, not tooling | Specs (content), `reviews.json` (trust ledger), egress security analysis, governance docs, marketplace site | General tooling (Python shrinks to security analysis + site build) |

## Boundary rules

1. **One validation implementation, two callers.** `shared::step_spec` is the
   only authority. `buz-plugin validate` is authoring-time *feedback*
   (advisory); the Manager re-runs the same validation at save and at pool
   enable — consumption-time *enforcement*. The split is by moment, never by
   implementation.
2. **Runtime binaries carry no authoring tools.** `buz-manager` is a pure
   operations control plane. The `step-validate` subcommand is retired once
   the plugins CI switches to `buz-plugin validate`.
3. **The content repository publishes governance, not tooling.** In the
   plugins repository Python shrinks to `egress.py` (security static analysis
   — coupled to the human review baseline, deliberately not rewritten) and
   the site build. `gen_index.py` / `gen_reviews.py` are superseded by
   `buz-plugin index` / `review`.
4. **The toolchain never re-invents the contract.** `buz-plugin` lives in the
   `buz` workspace precisely so it compiles against `shared` — a separate
   repository would break rule 1.

## Getting the tool

`buz-plugin` ships with Buz as a single static binary (same release as the
Manager and Runner):

1. **Download** — grab `buz-plugin-<os>-<arch>` from the
   [latest Buz release](https://github.com/schiplat/buz/releases), unpack, and
   put it on `PATH`.
2. **Build from source** — `cargo build --release -p plugin-cli` produces
   `target/release/buz-plugin`.

Verify with `buz-plugin --version`; `buz-plugin --help` lists the four
commands below.

## The `buz-plugin` CLI

| Command | Purpose |
|---------|---------|
| `buz-plugin init <namespace>/<id>` | Scaffold `plugins/<ns>/<id>/` per the [publishing layout](./plugin-contract/#publishing-layout): a `0.1.0.yaml` spec, README, and next-step hints |
| `buz-plugin validate <path…>` | Validate spec files/directories with the exact gate the Manager applies at save (`shared::step_spec`), including duplicate `(id, version)` detection |
| `buz-plugin index` | Generate the source-root `plugins/index.json` (+ per-plugin `meta.json` + `revision.json` sentinel) — supersedes `gen_index.py`. `--check` verifies instead of writing |
| `buz-plugin mirror check <source> <mirror>` | Compare the namespace revision sentinel between canonical origin and mirror (tiny GETs); falls back to a full index byte-compare when a sentinel is missing — the same protocol the Manager's *Check mirror* action speaks |

Index output is **byte-deterministic** (sorted entries, ordered fields,
two-space JSON + trailing newline). CI regenerations are byte-identical, and
mirror drift checks can therefore compare bytes safely.

That contract is pinned by golden files (`crates/plugin-cli/tests/fixtures/`):
the generator's output is compared byte-for-byte against committed fixtures
covering multiple plugins and versions, so a change to indentation, key order,
entry sorting, or any field value fails the test with a diff. Changing the
contract deliberately means updating the golden files in the same commit.

### CI gate: `--check`

`buz-plugin index --check` plans the manifests exactly as generation would,
compares them with what is committed, and exits non-zero on any difference —
including a `meta.json` left behind by a deleted spec. The plugins
repository's CI is intended to run:

```bash
buz-plugin index --check --revision "$GITHUB_SHA" --updated-at "$(git log -1 --format=%cI)"
```

which turns "the committed manifests must equal what the tool produces" into a
hard gate instead of a hand-run diff. Until the released binary is wired into
that workflow, the Python generator keeps that role.

## Validation at two moments

- **Authoring moment** (`buz-plugin validate` / `index`): fast, local, no
  server. Catches broken specs before review.
- **Consumption moment** (`buz-manager`): pipeline save re-resolves and
  re-validates everything it locks; pool enable runs the same gate
  ("收录即校验"). This is the security boundary — CI validation is advisory,
  the Manager is the enforcement point.

Because both moments call one function, a plugin tool and a Manager of the
same contract version cannot disagree.

## Rollout

1. **Phase 1** — `crates/plugin-cli` with the four commands; the plugins
   repository's CI switches `gen_index.py` → `buz-plugin index` once a
   release binary exists.
2. **Phase 2** — template repository + reusable CI action for community
   plugin repositories, built on the CLI.
3. **Phase 3 (optional)** — port the review ledger tooling (`review`
   subcommand); study egress-analysis parity (stays Python until proven).

Retirement schedule: `buz-manager step-validate` and `gen_index.py` are
removed after Phase 1 has run for one release cycle; the enforcement logic in
the Manager is never removed.
