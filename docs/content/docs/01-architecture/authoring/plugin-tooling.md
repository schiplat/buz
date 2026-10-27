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

1. `plugins/index.json` — the signed catalog header (`sequence`, `issued_at`)
   plus every `(id, version)` with its content `sha256` and scalar metadata,
   sorted deterministically
2. `plugins/<ns>/<id>/meta.json` — per plugin, for the Manager's
   single-plugin refresh
3. `plugins/revision.json` — **only with `--revision`**, always last

`--revision` / `--updated-at` are CI inputs (commit sha + commit time); a
local run without `--revision` never fabricates the revision sentinel, and
without `--updated-at` the header takes the current time:

```bash
buz-plugin index --revision "$(git rev-parse --short HEAD)" \
  --updated-at "$(git log -1 --format=%cI)"
```

`--no-meta` skips step 2 (dry runs). Generation validates every spec first
and refuses to write anything when one fails.

Index output is **byte-stable for unchanged entries**: the header's
`sequence` advances only when an entry changes, and `issued_at` is reused in
that case, so a no-op regeneration reproduces the committed bytes — and the
committed signature still matches. `--sequence N` pins the counter for
publishers that maintain it themselves (0 is rejected).

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

**Status:** the plugins repository's publish path has run on `buz-plugin` since
Buz 1.11.0 — generation, signing, and this `--check` re-verification — and tag
snapshots attach the signed index too. Two Python call sites remain on
purpose: the per-PR generator smoke test (a required check must not depend on
fetching a binary from a private release, because fork PRs receive no
credentials) and the fallback used when that download is unavailable. Both
generators write the signed `sequence` header since 1.12.0; a Manager that
requires signatures refuses an index without it, so publish with a 1.12.0+
tool **before** upgrading the Manager (or before flipping *Require signed
index* on).

### Sign the index — operation steps

Signing binds the publisher (you) to the exact index bytes, so a mirror or
anyone on the network cannot rewrite the index *and* the hashes inside it. The
Manager enforces it only when **Settings → Index signature → Require signed
index** is on — but publishing signatures is worth doing first, so turning the
switch on later needs no republishing.

#### Step 1 — create the keypair (once per namespace)

```bash
buz-plugin keygen --key-id acme-2026 --out signing-key.json
```

`--key-id` is free-form and recorded in every signature; use something like
`<org>-<year>` so a rotation is identifiable later.

The output has two halves with opposite handling:

| Half | Secrecy | Where it belongs |
|------|---------|------------------|
| `secret_key` | **private** — whoever holds it can sign any index for your namespace | a CI secret (e.g. `BUZ_PLUGIN_SIGNING_KEY`) or a secret manager; mode 600 if it must exist on disk; never in a repository, a log, or a chat |
| `public_key` | public | every Manager that consumes the source (`plugin_index_public_key`), plus README/docs |

`keygen` enforces the habit: it writes the file mode 600 with a warning, and
warns on stderr when printing to stdout. The plugins repository ignores
`signing-key*.json`.

#### Step 2 — put the secret where CI can read it

```bash
gh secret set BUZ_PLUGIN_SIGNING_KEY < signing-key.json
rm signing-key.json        # the secret now lives only in the secret store
```

Use it **only in jobs triggered by pushes/tags** — a `pull_request` workflow
(especially from forks) must never receive the signing key.

#### Step 3 — sign on every publish

```bash
buz-plugin index \
  --revision "$GITHUB_SHA" \
  --updated-at "$(git log -1 --format=%cI)" \
  --sign-key /tmp/signing-key.json
```

Writes `index.json.sig` next to `index.json` and prints `signed … (key id:
acme-2026)`. Commit both files. In GitHub Actions:

```yaml
- name: Generate and sign the index
  env:
    # Passed through env (not interpolated into the script) so the secret is
    # never part of the rendered command, and masked in logs.
    BUZ_PLUGIN_SIGNING_KEY: ${{ secrets.BUZ_PLUGIN_SIGNING_KEY }}
  run: |
    set -euo pipefail
    umask 077                       # the key file never exists as world-readable
    KEY="$RUNNER_TEMP/signing-key.json"
    printf '%s' "$BUZ_PLUGIN_SIGNING_KEY" > "$KEY"
    trap 'rm -f "$KEY"' EXIT        # no copy survives the job
    buz-plugin index --revision "$GITHUB_SHA" \
      --updated-at "$(git log -1 --format=%cI)" --sign-key "$KEY"
    buz-plugin index --check --sign-key "$KEY" \
      --revision "$GITHUB_SHA" --updated-at "$(git log -1 --format=%cI)"
```

#### Step 4 — verify in CI (catches a stale signature)

```bash
buz-plugin index --check --sign-key /tmp/signing-key.json \
  --revision "$GITHUB_SHA" --updated-at "$(git log -1 --format=%cI)"
```

This re-checks both the manifests **and** the signature, and the two failures
are distinguishable:

| What changed | Failure |
|---|---|
| `index.json` differs from what the specs produce (hand edit, forgotten regeneration) | `index.json is out of date — run buz-plugin index … and commit the result` |
| manifests are current but the committed `index.json.sig` no longer matches them (regenerated without `--sign-key`, rotated key, edited signature) | `index.json.sig is out of date — re-run without --check to sign` |

That is the CI gate: the committed signature must match the committed index.

#### Step 5 — configure the Manager

Settings → **Index signature**:

| Setting | Value |
|---------|-------|
| *Index signature public key* | the `public_key` value from Step 1 (base64) |
| *Require signed index* | on — after the source publishes a signature |

With the switch on, a sync fails when the source is unsigned (`index is not
signed — publish index.json.sig …`), when the signature was made by another key
(`does not match the configured public key`), or when the index changed after
signing (`index does not match its signature — signed sha256 …, received …
(index.json and index.json.sig come from different revisions)`). The last one
is the mirror-attack case.

Per-plugin `meta.json` carries no signature, so a signed source refreshes
through the full index instead of the meta shortcut.

#### Rollback protection

A signature proves *authenticity*, not *freshness*: an attacker who kept an
older signed `index.json` and its templates can serve that pair forever, and
every signature and hash check still passes. The signed `sequence` is the
ordering that closes this:

| Incoming sequence | Manager verdict |
|---|---|
| higher than the accepted one | accept and remember it |
| equal, byte-identical index | accept (idempotent re-sync) |
| equal, different bytes | refuse — two catalogs cannot share one sequence |
| lower than the accepted one | refuse — a validly signed older catalog is being replayed |
| missing (signed index) | refuse — regenerate with a `buz-plugin` that writes `sequence` |

Only an index that passed signature verification moves the watermark, so a
deployment without *Require signed index* never records one. The first
verified sync establishes the baseline; protection starts there. When a
publisher legitimately restarts its numbering, **Reset watermark** on the
source (audited) forgets it and the next sync re-establishes it. The
watermark is also visible on the Sources page (`index sequence N · issued …`).

#### Step 6 — mirrors copy the signature

A mirror is a plain file copy: `index.json`, `index.json.sig`, `reviews.json`,
`revision.json`, the specs and `meta.json` files. If `index.json.sig` is
missing, Managers with the switch on reject the mirror. Publish content first
and the sentinel last, and treat the signature as part of the content.

#### Rotating the key

1. `buz-plugin keygen --key-id acme-2027 --out signing-key-2027.json`.
2. Put the new public key into every Manager's *Index signature public key*
   **before** you start signing with it (a Manager trusts one key at a time).
3. Switch the CI secret to the new private key and publish; signatures now
   carry `key_id: acme-2027`, which identifies them in logs and in the error
   messages above.
4. Delete the old key material.

If a Manager must keep accepting both during the window, leave *Require signed
index* off until the switch completes — or ask for multi-key support, which is
not implemented yet.

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
