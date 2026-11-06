---
title: Plugin Contract
weight: 7
description: "The versioned interface between the Buz runtime and step-template plugins"
---

# Plugin Contract v1

A step-template plugin is a directory of YAML specs (plus optional tool
images) that a Manager resolves against the local plugin index. The plugin and
the Buz runtime interact only through the interface documented here — there
is no plugin runtime, SDK, or library dependency. This page is the normative
source for that interface; the plugins repository documents packaging,
publishing, and governance on top of it.

**Contract version:** v1, current as of Buz 1.7.x. Breaking changes bump the
contract version and are called out in release notes. Templates can guard
against incompatibility with `requires_buz` (see [Version gate](#version-gate)).

## Workspace

- The Runner's workspace directory is bind-mounted into every Docker step at
  `/buz/workspace` (`CONTAINER_WORKSPACE`). Shell steps run on the Runner
  host with the workspace as their working directory.
- Relative input paths (sources, `workdir`, config destinations) resolve
  inside the workspace; template scripts must reject paths that escape it.
- State between steps travels through workspace files — every step is an
  isolated process/container, and the environment never carries over.

## Invocation

- Docker engine: the image is started with `docker run`, executing the
  template's `run:` block via `sh -elc`. Tool images therefore set
  `CMD ["sh"]` and no command-consuming `ENTRYPOINT`.
- Shell engine: the `run:` block executes on the Runner host with the same
  `sh -elc` convention — POSIX sh only.
- SSH engine: the `run:` block is executed on the target host with `INPUT_*`
  and secret values exported through the command prefix.
- `engines:` (optional, template spec) declares extra engines the run block
  supports; a call site may then set `engine:` to override (shell ↔ ssh).
  Incompatible overrides fail at claim time.

## Inputs

- `with:` values arrive as `INPUT_<NAME>` environment variables (name
  uppercased; multi-line values preserved verbatim).
- Types (`string`, `number`, `boolean`, `choice`, `list`), `required`,
  `default`, `options`, and `pattern` are validated by the Manager at
  pipeline save — templates must not re-implement type validation.
- Inputs named `source`/`workdir`/similar path-like values must be
  workspace-relative; the template validates and rejects escapes.

## Metadata environment

Steps receive job metadata as environment variables (empty values are not
set):

| Variable | Content |
|----------|---------|
| `BUZ_JOB_ID` | Job identifier |
| `BUZ_JOB_URL` | Absolute URL of the job detail page |
| `BUZ_PIPELINE_NAME` | Pipeline name |
| `BUZ_TEAM` | Team that owns the pipeline |
| `BUZ_TRIGGER` / `BUZ_TRIGGERED_BY` | Trigger type and actor |
| `BUZ_RUN_NUMBER` / `BUZ_RUN_ATTEMPT` | Per-pipeline build number (1-based, like GHA `github.run_number`) and the re-run counter — useful for image tags like `app:$BUZ_RUN_NUMBER` and "build #N" messages |

| `BUZ_GIT_SHA` / `BUZ_GIT_BRANCH` | Normalized from `GIT_SHA` / `GIT_BRANCH` inputs on webhook triggers |

| `BUZ_ENV_CHANGED`, `BUZ_ENV_CHANGED_KEYS` | Render results, when the step declares a `config.render` |

## Secrets

- Secret values referenced as `${secrets.NAME}` are injected as environment
  variables of the individual step only, live in Runner memory, are never
  written to disk, and are never sent back to the Manager.
- Log masking (Aho-Corasick) redacts secret values in streamed logs in real
  time. Templates must accept secrets via `INPUT_*`/env — never via
  command-line arguments or interpolated URLs where they would leak.
- The public Marketplace registry additionally requires network-touching
  inputs to be declared in its reviewed egress baseline.

## Egress

The Runner enforces host-level egress policy; template `run:` blocks may not
add destinations beyond what the declared inputs allow. Shell steps'
network calls are recorded against the egress baseline by a static checker;
calls made inside tool images are outside its view and must be reviewed
separately (documented in the plugins repository).

## Version gate

Specs may declare `requires_buz: ">=X.Y.Z"` (or an exact `X.Y.Z`). The
Manager refuses to save a pipeline whose template requires a newer Buz than
the running Manager. The plugins repository pins the Buz version its CI
validates against, so a contract change surfaces as a failing plugins build
rather than a silent runtime break.

## Publishing layout

A plugin repository publishes **one source root per namespace** — the
namespace is the publishing/trust boundary, and the manifests live at the
root of that boundary:

```
your-plugins/
└── plugins/                  ← source root (what a Manager source URL points into)
    ├── index.json            ← sequence + issued_at + entries (id / version / file / sha256)
    ├── reviews.json          ← optional; reviewed-content manifest
    ├── revision.json         ← freshness sentinel (CI-written, last)
    ├── index.json.sig        ← optional detached signature over index.json
    ├── reviews.json.sig      ← optional; signature over reviews.json (same signing run)
    └── buz/                  ← namespace directory (specs)
        ├── http-check/
        │   ├── 1.0.0.yaml
        │   └── meta.json     ← generated; single-plugin refresh
        └── archive/
            └── 1.1.0.yaml
```

### The three manifests

The source root carries three generated files. Each answers a different
question — keeping them separate is what makes mirrors cheap, reviews
trustworthy, and the index byte-stable:

| file | answers | written by | read by |
|------|---------|------------|---------|
| `index.json` | *what exists* — every plugin version, its content hash and location | CI, generated from the specs (`buz-plugin index` / `gen_index.py`) | Manager source sync (pool staging), the marketplace site, authoring-time input hints |
| `reviews.json` | *what humans approved* — the trust ledger | maintainers after human review (`gen_reviews`), CI keeps official entries fresh | Manager review badges and the `require_reviewed` gate, marketplace badges |
| `revision.json` | *what is freshest* — a drift sentinel | CI, written **last** (`buz-plugin index --revision`) | Manager *Check mirror*, mirror sync scripts, `buz-plugin mirror` |

- **`index.json` is the catalog.** Every entry pins the template's content
  sha256, so resolution locks at save time regardless of which host served
  the file. It contains **no review information**. Its header carries two
  fields above `steps`:

  ```json
  { "sequence": 7, "issued_at": "2026-10-27T10:14:00+08:00", "steps": [ … ] }
  ```

  `sequence` is a strictly increasing catalog version and `issued_at` is when
  that version was issued. Both sit **inside** the signed bytes, and the
  generator only advances them when the entries change — regenerating an
  unchanged catalog reproduces the same bytes (and therefore the same
  signature). A Manager that has already accepted sequence 7 refuses any
  index claiming a lower one, and refuses two different catalogs sharing one
  sequence: a validly signed *older* index (with its equally valid template
  hashes) can no longer be replayed. Set `--sequence` explicitly only when
  you maintain the counter yourself.
- **`reviews.json` is the trust ledger.** Entries are content hashes
  recorded after human review; the review gate (`require_reviewed`)
  rejects any template whose hash is absent from it. It lives per source
  root so official review authority and community entries never share a
  ledger. A rewritten ledger could mark attacker-chosen hashes as
  reviewed, so under a trust anchor it is trusted only with its own
  signature (below).
- **`index.json.sig` is a detached Ed25519 signature over the exact index
  bytes** (`{"alg":"ed25519","key_id":"…","signature":"<base64>","sha256":"<hex>"}`).
  The content hashes inside an unsigned index can be rewritten together with
  the files they pin, so the chain stays self-consistent; the signature is what
  lets a Manager detect a rewritten index, a mirror included. Generate it with
  `buz-plugin index --sign-key` and copy it alongside the index when mirroring.
  Authenticity is not freshness — the signed `sequence` supplies the ordering
  that makes a replay of an older signed index detectable.
- **`reviews.json.sig` signs the ledger the same way**, produced by the same
  `--sign-key` run. When *Require signed index* is on, a sync that fetches a
  ledger without a verifying signature fails — otherwise anyone who could
  rewrite `reviews.json` alone could grant themselves review approval.
- **`revision.json` is the freshness sentinel.** It carries
  `{"revision": "<commit sha>", "updated_at": "<iso8601>"}` and is written
  **last** — it commits the sync. Comparing two sentinels (a few hundred
  bytes) tells a mirror whether it is behind; only on mismatch does
  anything larger get pulled. It says nothing about review status.
- `file:` paths in `index.json` / `meta.json` are **relative to the source
  root**. The Manager joins them against the source base URL (the
  `index.json` URL minus its tail), so a source can be hosted anywhere — a
  static site, repo raw URLs, release assets, a self-hosted mirror — as
  long as the whole tree keeps the same shape.
- **One namespace per source root**: the Manager rejects an index whose
  entries span several namespaces. Third-party plugins therefore publish
  from their own repositories with this same layout and register as their
  own Step Source, with their own curation policy.
- Generate the manifests from the specs — never hand-edit hashes. The
  official repository's `scripts/gen_index.py` / `scripts/gen_reviews.py`
  and the `buz-plugin` CLI are the reference implementations.
- The Step Source URL registered with the Manager is the index URL, e.g.
  `https://plugins.example.com/plugins/index.json`.

