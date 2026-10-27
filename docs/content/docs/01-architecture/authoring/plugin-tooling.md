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
| `shared` crate | The contract: what is a legal plugin | `plugin_spec` types + `validate_plugin_spec` (single authority) | Any CLI or I/O |
| `buz-plugin` CLI | What authors, publishers, and mirror operators do | `init`, `validate`, `index`, `mirror check` | Runtime enforcement — it is feedback, not a gate |
| `buz-manager` | Enforcing validation at the moment it consumes a template | Save-time resolution & locking, enable-time verification (`verify_entry`), the review gate, mirror failover | Authoring tools — `step-validate` retires once `buz-plugin` takes over |
| plugins repository | Content and governance, not tooling | Specs (content), `reviews.json` (trust ledger), egress security analysis, governance docs, marketplace site | General tooling (Python shrinks to security analysis + site build) |

## Boundary rules

1. **One validation implementation, two callers.** `shared::plugin_spec` is the
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

1. **Download** — grab `buz-plugin-<version>-<platform>.tar.gz` from the
   [latest Buz release](https://github.com/schiplat/buz/releases) (platforms:
   `linux-x86_64`, `linux-aarch64`, `macos-aarch64`; each with a `.sha256`),
   unpack the single binary, and put it on `PATH`.
2. **Build from source** — `cargo build --release -p plugin-cli` produces
   `target/release/buz-plugin`.

Verify with `buz-plugin --version`; `buz-plugin --help` lists the four
commands below.

## The `buz-plugin` CLI

| Command | Purpose |
|---------|---------|
| `buz-plugin init <namespace>/<id>` | Scaffold `plugins/<ns>/<id>/` per the [publishing layout](./plugin-contract/#publishing-layout): a starter spec plus README |
| `buz-plugin validate <path…>` | Validate spec files/directories with the exact gate the Manager applies at save (`shared::plugin_spec`), including duplicate `(id, version)` detection |
| `buz-plugin index` | Generate the source-root `plugins/index.json` (+ per-plugin `meta.json` + `revision.json` sentinel) — supersedes `gen_index.py`. `--check` verifies instead of writing |
| `buz-plugin mirror <source> <mirror>` | Compare the revision sentinel between canonical origin and mirror (two tiny GETs); falls back to a full index byte-compare when a sentinel is missing — the same protocol the Manager's *Check mirror* action speaks |

Run `buz-plugin <command> --help` for the full flag list. Every command
exits non-zero on failure, so they drop straight into `set -e` scripts.

## Usage

### Scaffold a plugin — `init`

Run from the repository root that should contain `plugins/`:

```bash
buz-plugin init alice/hello \
  --author alice \
  --source-repo https://github.com/alice/buz-steps \
  --description "Say hello to someone."
```

Creates `plugins/alice/hello/0.1.0.yaml` and a matching `README.md`, then
re-runs the contract gate on the result and prints what still needs filling
in. Notes:

- `--publisher` defaults to `community`; that mode requires `--author` and
  `--source-repo` (attribution rules in the plugin contract).
- `--engine` (`shell` by default), `--version` (`0.1.0`), and `--dir`
  (repository root, `.` by default) are optional.
- The namespace `buz` is reserved for officially maintained plugins — pick
  your own handle.
- An existing spec is never overwritten: re-running `init` fails instead.

### Validate specs — `validate`

```bash
buz-plugin validate plugins/alice/hello          # a directory
buz-plugin validate plugins/alice/hello/0.1.0.yaml   # a single file
```

Prints one `OK`/`FAIL` line per spec (with the field-level reasons on
failure) and a `passed/total` summary. This is the same function the Manager
runs at save, so "valid here" means "accepted there" — except that the
Manager additionally enforces the review gate and resolves inputs against
your deployment.

### Generate the manifests — `index`

Run in the repository root; `--plugins-dir` defaults to `plugins`:

```bash
buz-plugin index
```

Writes, in this order:

1. `plugins/index.json` — every `(id, version)` with its content `sha256` and
   scalar metadata, sorted deterministically
2. `plugins/<ns>/<id>/meta.json` — per plugin, for the Manager's
   single-plugin refresh
3. `plugins/revision.json` — **only with `--revision`**, always last

`--revision` / `--updated-at` are CI inputs (commit sha + commit time); a
local run without them never fabricates a version marker:

```bash
buz-plugin index --revision "$(git rev-parse --short HEAD)" \
  --updated-at "$(git log -1 --format=%cI)"
```

`--no-meta` skips step 2 (dry runs). Generation validates every spec first
and refuses to write anything when one fails.

Index output is **byte-deterministic** (sorted entries, ordered fields,
two-space JSON + trailing newline). CI regenerations are byte-identical, and
mirror drift checks can therefore compare bytes safely.

That contract is pinned by golden files (`crates/plugin-cli/tests/fixtures/`):
the generator's output is compared byte-for-byte against committed fixtures
covering multiple plugins and versions, so a change to indentation, key order,
entry sorting, or any field value fails the test with a diff. Changing the
contract deliberately means updating the golden files in the same commit.

### Verify manifests in CI — `index --check`

```bash
buz-plugin index --check \
  --revision "$GITHUB_SHA" --updated-at "$(git log -1 --format=%cI)"
```

Plans the manifests exactly as generation would, compares them with what is
committed, and exits non-zero on any difference — an out-of-date
`index.json`, a missing `meta.json`, or a `meta.json` left behind by a
deleted spec. On success it prints `manifests up to date (N file(s) checked)`;
on failure it names each offending file and what to run. This is the
intended plugin-repository gate: the committed manifests must equal what the
tool produces.

### Sign the index — `keygen` + `index --sign-key`

A source that must stay trustworthy even when served through a mirror signs
its index:

```bash
# once: create a keypair (secret stays in CI; hand the public half to operators)
buz-plugin keygen --key-id acme-2026 --out signing-key.json

# every publish: sign the index that was just generated
buz-plugin index --revision "$GITHUB_SHA" --updated-at "$(git log -1 --format=%cI)" \
  --sign-key signing-key.json
```

The signature lands in `index.json.sig`. The Manager verifies it only when
**Settings → Index signature → Require signed index** is on, against the
configured public key: an unsigned source, or one whose index was rewritten
(the mirror attack — change the index *and* the hashes inside it), then fails
with `index signature rejected`. `index --check --sign-key` re-verifies the
signature in CI too. Per-plugin `meta.json` is unsigned, so signed sources
refresh through the full index instead.

### Check a mirror

```bash
buz-plugin mirror \
  https://plugins.example.com/plugins/index.json \
  https://mirror.example.net/plugins/index.json
```

Compares `revision.json` on both sides. Human output:

```text
# identical sentinels — exit 0
in-sync (revision 8d367d8)
detail: 2026-10-27T09:30:00Z

# drifted — exit 1
out-of-sync — canonical rev 8d367d8, mirror rev 3f2a91c; re-sync the mirror
detail: 2026-10-27T09:30:00Z
```

`canonical-unreachable` and `mirror-unreachable` also exit non-zero, with the
underlying error on the `detail` line. Add `--json`
(`{"status","canonical_revision","mirror_revision","detail"}`) for monitoring
scripts.

## Validation at two moments

- **Authoring moment** (`buz-plugin validate` / `index`): fast, local, no
  server. Catches broken specs before review.
- **Consumption moment** (`buz-manager`): pipeline save re-resolves and
  re-validates everything it locks; pool enable runs the same gate
  ("收录即校验"). This is the security boundary — CI validation is advisory,
  the Manager is the enforcement point.

Because both moments call one function, a plugin tool and a Manager of the
same contract version cannot disagree.

### What a sync downloads

A source's index lists every version, but a sync does **not** fetch them all:

1. A `requires_buz` declared in the index is evaluated first — no download. An
   entry this Manager is too old for is recorded as incompatible (with the
   requirement in the message) straight away.
2. The **newest version of each plugin is downloaded and fully validated**; the
   rest enter the pool with an unknown verdict.
3. An older version is downloaded the moment someone **enables it** (enable-time
   verification) or a pipeline **resolves a pin to it** — both gate the content
   the same way.

Downloads are cached by content hash (`data/plugin-index-cache/templates/`), so
repeat syncs and repeat resolutions cost nothing. The exception is a legacy
source whose index carries no `requires_buz`: with no cheap gate to rule on an
entry, every version is still verified at sync time — otherwise a compatible
but not-newest version could never be auto-enabled.

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
