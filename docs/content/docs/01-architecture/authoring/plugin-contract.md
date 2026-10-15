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
