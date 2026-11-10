---
title: YAML-First Pipeline Authoring
weight: 10
description: "The pipeline row stores the authoritative YAML; git receives read-only export snapshots"
aliases:
  - /docs/01-architecture/yaml-first-authoring/
---

{{< callout type="note" >}}**Design record — implemented (2026-08).**{{< /callout >}}

**Status:** Updated (2026-11). Form UI stays hidden. The online YAML editor writes the definition straight into the pipeline row; no file is involved. A directory of YAML files can be imported or exported with the CLI, but it is never watched and never authoritative.

## Decision

1. **YAML is the only orchestration source of truth.** Pipelines are authored as GHA-style YAML (same shape as `example/pipelines/*.yml` and [Pipeline Format](./pipeline-format.md)).
2. **The New/Edit Pipeline Form exits the main authoring path.**
3. **Storage: the pipeline row is authority.** `pipelines.yaml_source` holds the exact YAML bytes the validator accepted; `yaml_path` keeps the origin basename. The DB also holds the runtime projection (tasks, secret/var bindings, job snapshots). There is no second authoring source: the API/UI is the only writer, and git receives **read-only export snapshots**.
4. **Keep non-orchestration UI:** Trigger dialog, Job detail/logs, Infrastructure (Secrets / Variables / Hosts / Teams), Runner policy, RBAC, audit.

## Runtime model

```
UI / API Save ──► parse_and_validate ──► pipelines.yaml_source   (authority)
                                              │
                                              ├── tasks / step_resolutions / bindings
                                              │
                                     buz-manager pipeline export --dir
                                              ▼
                                        git snapshot (read-only)
```

| Layer | Role |
|-------|------|
| `pipelines.yaml_source` | Authority — the definition exactly as validated |
| Save API | Validate → store definition + projection in one write path |
| `buz-manager pipeline export` | Write one `<origin>.yml` per pipeline; for git review/audit |
| `buz-manager pipeline import` | Read a directory once; per-file upsert, never deletes |
| `yaml_scan_dir` | Import/export location only — not watched, not written by the server |

## API

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/pipelines/yaml-files` | List files + linked pipeline ids |
| `POST` | `/pipelines/yaml-files` | Create file + upsert (`filename` optional → slug from `name:`) |
| `GET` | `/pipelines/yaml-files/{filename}` | Read file content |
| `PUT` | `/pipelines/yaml-files/{filename}` | Overwrite + upsert; optional `pipeline_id` migrates Form/API → file |
| `DELETE` | `/pipelines/yaml-files/{filename}` | Delete file + pipeline row |
| `GET` | `/pipelines/{id}/yaml-source` | Load authority YAML for a `source=yaml` pipeline |

Filenames are bare basenames only (`^[A-Za-z0-9_-]+\.(yml|yaml)$`); the stem must **not** contain `.` (dots are reserved for the extension). Path separators / `..` are rejected. The UI slugifies spaces and other characters to `-`.

**Validate / Save gates (Manager DB):** `/pipelines/validate` and YAML create/update reject when referential checks fail — unknown `team:` / `owner_group:` slug, missing same-team secret names referenced via `${secrets.X}` / purpose slots / lists, or missing platform `variables:` names. Soft fallback of unknown teams to `default` is **removed**.

**Field registration:** unknown keyword keys at every nesting level are rejected (`deny_unknown_fields`). New fields must be registered in `pipeline_yaml.rs` before use — including top-level `notify:` (`NotifyChannelYaml`) — see [Pipeline Format → Field registration](./pipeline-format.md#field-registration-required-before-use).

**Auth / consistency:**

- Create/PUT authorize `PipelineWrite` on the **serde-parsed** YAML `team:` **before** any write (no post-write orphan on team migrate).
- `/pipelines/validate` is viewer-scoped: without `TeamView` on the target team, errors are generic (`team not found or not accessible`) — no cross-team secret-name oracle.
- `PUT /pipelines/{id}` (structured body) is rewritten into YAML and stored — every pipeline has one write path.
- Upsert **does not** force `is_active=1` (a manual deactivate sticks).

**Name model (flat namespace):**

- `pipelines.yaml_path` stores the **origin basename** (e.g. `demo.yml`), never absolute paths.
- A partial unique index on `(yaml_path)` enforces one pipeline per definition name; two concurrent creates converge on one winner (the other gets **409**).
- Filename rules: bare `stem.ext` with stem `[A-Za-z0-9_-]+` and `.yml`/`.yaml`.

**Trigger / Definition:**

- A trigger reads the stored definition and its projection. A definition cannot be saved invalid, so there is nothing to re-sync at trigger time.
- `GET /pipelines/{id}` includes `yaml_sync` for the Definition banner. With the database authoritative the only states are `in_sync` and `missing_file` (a legacy row whose definition was never backfilled).
- The Definition tab reads `yaml-source` from the database.

**Optimistic concurrency:**

- GET yaml-source / yaml-files returns `etag` (content hash).
- PUT accepts body `etag` or header `If-Match`; mismatch → **409**.

## UI

| Control | Behavior |
|---------|----------|
| Pipelines → **New Pipeline** | Wide YAML editor (highlight + live lint + autocomplete) → **Create**; first visit shows a coachmark pointing to **Quick YAML Reference** (dismissed once via `localStorage`) |
| Pipelines → row **Edit** | Same editor; `source=yaml` loads file; legacy API pipelines migrate on Save |
| Pipeline Detail → **Edit YAML** | Opens editor in-place (no list navigation) |

Editor: CodeMirror 6 — YAML syntax highlighting, debounced `parse_and_validate` gutter diagnostics, completions for keywords / `engine:` / `${secrets.` / `${vars.` / Manager secret & variable names.

The Form authoring UI (step cards / Config Block form) has been **removed** from the frontend.

## Config

```toml
[pipeline]
# Import/export location only. The server never watches or writes it.
yaml_scan_dir = "./pipelines"
# Retired: accepted for config compatibility, no longer used.
yaml_scan_interval_seconds = 30
```

```bash
# Read-only snapshot for a git repository (stable diffs)
buz-manager pipeline export --dir pipelines/

# One-shot import; per file, never deletes. --dry-run validates only.
buz-manager pipeline import --dir pipelines/
```

See [Configuration](/docs/03-operations/configuration/).

## Notes

- **Form storage is retired** (single-source): `POST`/`PUT /pipelines` render the request into YAML and store it — every pipeline is `source=yaml` with a stored definition. A startup backfill stores YAML for pre-existing rows once (legacy file first, then the relational projection; failures are logged and retried on the next start, and the row keeps serving from its projection meanwhile). Two pipeline names may share a slug — the second gets a `-2` suffixed file. Behavior tightened with the contract: a cron trigger without `on.cron.environment` is now rejected at create (the YAML schema always required it; the old form path was lax).
- Quick YAML Reference lives in the YAML editor dialog (Editor | Reference tabs), not a separate Form.

## Related

- [Pipeline Format](./pipeline-format.md) — YAML field reference
- [Secrets & Variables](/docs/01-architecture/authoring/secrets-variables/) — `env: KEY: ${secrets.NAME}`
- [Migrating from the kernel `config:` block](./migration-config-block.md)
