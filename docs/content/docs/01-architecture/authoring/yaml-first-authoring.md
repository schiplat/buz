---
title: YAML-First Pipeline Authoring
weight: 90
description: "YAML file/Git is the authority; Manager projects into DB; UI edits write back to the scan directory"
aliases:
  - /docs/01-architecture/yaml-first-authoring/
---

**Status:** Implemented (2026-08). Form UI stays hidden. Online YAML editor creates/updates/deletes files under Manager `pipeline.yaml_scan_dir` and immediately upserts the DB projection.

## Decision

1. **YAML is the only orchestration source of truth.** Pipelines are authored as GHA-style YAML (same shape as `example/pipelines/*.yml` and [Pipeline Format](./pipeline-format.md)).
2. **The New/Edit Pipeline Form exits the main authoring path.**
3. **Storage: File / Git directory is authority.** Manager mounts or syncs that directory (`yaml_scan_dir`). The UI writes back to that path. The DB holds the **runtime projection** only (tasks, secret/var bindings, job snapshots) — not a second authoring DSL.
4. **Keep non-orchestration UI:** Trigger dialog, Job detail/logs, Infrastructure (Secrets / Variables / Hosts / Teams), Runner policy, RBAC, audit.

## Runtime model

```
Git / directory  ──(mount or sync)──►  Manager yaml_scan_dir
        ▲                                      │
        │                              scanner + on-save upsert
        │                                      ▼
   UI Save (write file)                 SQLite projection
                                        (pipelines / tasks / bindings)
```

| Layer | Role |
|-------|------|
| File under `yaml_scan_dir` | Authority — create / edit / delete |
| Periodic scanner | Catch external Git/FS changes |
| Save API | Write file → `parse_and_validate` → **immediate** upsert (same code path as scanner) |
| DB | Runtime projection for dispatch, RBAC, jobs |

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

- Create/PUT authorize `PipelineWrite` on the **serde-parsed** YAML `team:` **before** any disk/DB write (no post-write orphan on team migrate).
- `/pipelines/validate` is viewer-scoped: without `TeamView` on the target team, errors are generic (`team not found or not accessible`) — no cross-team secret-name oracle.
- `PUT /pipelines/{id}` (legacy Form/API body) returns **409** when `source=yaml` — edit via YAML file APIs only.
- Scanner / Save upsert **does not** force `is_active=1` (deactivate sticks across rescans).
- File write uses temp + rename; upsert failure restores the previous file content when possible.

**Path model (flat scan dir):**

- `pipelines.yaml_path` stores the **basename only** (e.g. `demo.yml`), never absolute paths.
- Files must live **directly** under `yaml_scan_dir` — no private subdirectories; scanner skips dirs and hidden/`.*` files.
- Filename rules: bare `stem.ext` with stem `[A-Za-z0-9_-]+` and `.yml`/`.yaml`.

**Trigger / Definition sync:**

- Trigger re-reads the file, validates, and upserts the DB projection; invalid/missing YAML **blocks** the run.
- `GET /pipelines/{id}` includes `yaml_sync` (`in_sync` | `stale` | `validation_failed` | `missing_file`) for the Definition banner.
- Definition tab **reads and parses the on-disk file** (via `yaml-source` + validate) for display; invalid/missing shows an alert and does **not** fall back to the DB projection as authority.

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
yaml_scan_dir = "./pipelines"          # mount Git checkout or shared volume here
yaml_scan_interval_seconds = 30        # backup sync for external edits
```

See [Configuration](/docs/03-operations/configuration/).

## Notes

- Form step-card authoring UI is **removed**; legacy `PUT /pipelines/{id}` body remains only for old `source=form` rows (409 once migrated to `source=yaml`).
- Quick YAML Reference lives in the YAML editor dialog (Editor | Reference tabs), not a separate Form.

## Related

- [Pipeline Format](./pipeline-format.md) — YAML field reference
- [Secrets Access (GHA-style)](/docs/01-architecture/authoring/secrets-access/) — `env: KEY: ${secrets.NAME}`
- [Config Block](./config-block.md)
