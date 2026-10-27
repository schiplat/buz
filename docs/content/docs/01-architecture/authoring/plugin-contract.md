---
title: Plugin Contract
weight: 7
description: "The versioned interface between the Buz runtime and step-template plugins"
---

# Plugin Contract v1

A step-template plugin is a directory of YAML specs (plus optional tool
images) that a Manager resolves against a local step index. The plugin and
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
  and secret values exported through the command prefix (`BUZ_REGISTRY_HOST`,
  `BUZ_REGISTRY_USER`, `BUZ_REGISTRY_PASSWORD` accompany a step-level
  `registry_secret`).
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
| `BUZ_CONFIG_URL` | Pre-signed config URL, when the step declares a `config.source` |
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
    ├── index.json            ← id / version / file / sha256 (+ scalar meta)
    ├── reviews.json          ← optional; reviewed-content manifest
    ├── revision.json         ← freshness sentinel (CI-written, last)
    └── buz/                  ← namespace directory (specs)
        ├── http-check/
        │   ├── 1.0.0.yaml
        │   └── meta.json     ← generated; single-plugin refresh
        └── archive/
            └── 1.1.0.yaml
```

- `file:` paths in `index.json` / `meta.json` are **relative to the source
  root**. The Manager joins them against the source base URL (the
  `index.json` URL minus its tail), so a source can be hosted anywhere — a
  static site, repo raw URLs, release assets, a self-hosted mirror — as
  long as the whole tree keeps the same shape.
- **One namespace per source root**: the Manager rejects an index whose
  entries span several namespaces. Third-party plugins therefore publish
  from their own repositories with this same layout and register as their
  own Step Source, with their own curation policy.
- `reviews.json` is fetched from the source root next to the index and
  provides the review badges (and the `require_reviewed` gate input).
  Keeping it per source root is what keeps official review authority
  separate from community entries.
- `revision.json` carries `{"revision": "<commit sha>", "updated_at":
  "<iso8601>"}`, written last by CI. Mirrors (and the Manager's *Check
  mirror* action) compare sentinels — two tiny GETs — to detect drift
  before pulling anything larger. Publish content first and the sentinel
  last — it commits the sync. The index itself stays byte-deterministic:
  volatility lives in the sentinel, not in the index.
- Generate the manifests from the specs — never hand-edit hashes. The
  official repository's `scripts/gen_index.py` / `scripts/gen_reviews.py`
  and the `buz-plugin` CLI are the reference implementations.
- The Step Source URL registered with the Manager is the index URL, e.g.
  `https://plugins.example.com/plugins/index.json`.

