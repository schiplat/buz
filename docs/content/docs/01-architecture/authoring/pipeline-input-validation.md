---
title: Pipeline Input Validation
weight: 80
description: "Declarative manual-input validation — type / pattern / options / host / required, enforced at trigger time before a job is queued"
---

## Overview

Manual trigger inputs (`on.manual.inputs`) historically were **free-form**: the UI prefilled defaults, but nothing stopped a user from triggering with a malformed `tag`, an unregistered host, or a value outside an expected set. Teams coped by hand-writing shell `case` / regex guards in the first step:

```yaml
- name: Validate inputs
  engine: shell
  run: |
    case "$INPUT_TAG" in
      ''|*[!A-Za-z0-9._-]*)
        echo "ERROR: invalid tag '$INPUT_TAG'" >&2
        exit 1
        ;;
    esac
```

**Buz now supports declarative input validation.** Constraints are declared in the pipeline YAML and enforced by the **Manager at trigger time** — the job is never queued when a value violates a constraint. Shell guards can be deleted from step definitions.

| Capability | Where it lives |
|------------|----------------|
| Constraint declaration | `on.manual.inputs.<name>` → `type` / `pattern` / `options` / `required` / `default` |
| Enforcement | Manager `do_trigger` (manual + webhook + cron share the same code path) |
| Field-level error display | Trigger dialog highlights offending fields in red |

---

## Syntax

Input-level constraint fields are **optional** — existing input definitions keep working unchanged. Independently, every pipeline must declare its deployment targets in a non-empty top-level `environments:` list; see [Pipeline Format](./pipeline-format/#environments-required-deployment-targets).

```yaml
on:
  manual:
    inputs:
      tag:
        description: ACR image tag
        required: true
        default: "0.1.0"
        type: string                    # optional — "string" is the default
        pattern: "^[A-Za-z0-9._-]+$"    # ECMAScript regex, whole-value match
      deploy_host:
        description: Buz Host name
        required: true
        type: host                      # must reference a registered Buz Host
      environment_name:
        description: Business input only; deployment environment is selected separately
        type: choice
        options: [staging, production]
      replicas:
        type: number
      dry_run:
        type: boolean
        default: "false"
```

### Field reference

| Field | Type | Meaning |
|-------|------|---------|
| `description` | string | Shown in the trigger dialog |
| `required` | boolean | Value must be provided (or filled from `default`) before the job is queued |
| `default` | string | Fallback merged when the field is omitted — **then validated like any user value** |
| `masking` | boolean | Existing behavior unchanged: redacted in logs/audit/snapshot, encrypted at rest in `trigger_params` |
| `type` | string | `string` (default) · `number` · `boolean` · `host` · `choice` |
| `pattern` | string | ECMAScript-compatible regex the resolved value must match |
| `options` | string[] | Allowed values — only meaningful with `type: choice` |

### `type` semantics

| `type` | Rule | Trigger dialog control |
|--------|------|------------------------|
| `string` (default) | Free-form text | text input |
| `number` | Must parse as `i64` | number input |
| `boolean` | Must be `true` / `false` (case-insensitive) | `true`/`false` dropdown |
| `host` | Value must equal a **registered Host name** (from the Hosts registry) | text input (validated at trigger) |
| `choice` | Value must be one of `options`; a `choice` without a non-empty `options` is a definition error | dropdown |

- An **unknown** `type` name (e.g. `date`) is reported as a definition error at trigger — it is **not** silently ignored.
- `pattern` works with any `type` (e.g. a `number` with a `^[0-9]{1,5}$` length guard).
- `options` without `type: choice` is ignored.

### YAML editor reminders

The YAML validation endpoint (`POST /api/v1/pipelines/validate`) emits **warnings** (non-blocking) for inputs whose `type` is not declared, so authors are nudged toward an explicit type while existing pipelines keep validating fine:

- `on.manual.inputs.<name>` with **no `type`** → warning: *"no 'type' declared — defaults to 'string'; declare 'type:' to enable number/boolean/host/choice validation and matching UI control"* (positioned on the input key line).
- `options` present but `type` is **not** `choice` → warning: *"'options' requires 'type: choice' to take effect — currently ignored"*.

These are **warnings, not errors** — pipelines that omit `type` still parse, import, and trigger exactly as before.

---

## Validation timing (fail-fast)

Validation runs inside the shared `do_trigger` path, which is used by all three trigger sources:

1. **Manual** — `POST /api/v1/pipelines/{id}/trigger`
2. **Webhook** — `POST /api/v1/hooks/{path}` (previously only checked `required`; now full validation)
3. **Cron** — scheduled runs (values come from defaults)

Order of operations:

1. Merge declared `default`s into the submitted inputs (a submitted value always wins over its default).
2. Validate every defined input against its constraints, including any merged default — **a malformed default is caught at trigger, never silently injected**.
3. On any violation, return **`422 Unprocessable Entity`** with field-level messages and **do not create a job** (`jobs` row count stays unchanged).

### Error format

```
invalid trigger inputs — tag: value does not match pattern '^[A-Za-z0-9._-]+$'; deploy_host: host 'ghost' is not registered
```

- One `name: reason` segment per offending field, `; `-separated.
- The frontend `parseApiError` recognizes the `invalid trigger inputs` prefix, renders each segment as a field badge + message, and **highlights the offending inputs in red** in the trigger dialog (both `PipelineDetail.vue` and the list-page trigger dialog).

---

## Example migration

A deploy pipeline previously shipped a `Validate inputs` shell step that hand-checked inputs for emptiness and format. It was replaced by declarations:

```yaml
on:
  manual:
    inputs:
      tag:
        description: Image tag
        required: true
        default: "0.1.0"
        type: string
        pattern: "^[A-Za-z0-9._-]+$"
      deploy_host:
        description: Buz Host name
        required: true
        default: "lab-server-01"
        type: host
```

> **Note:** `type: host` (including its `default`) must match the **`name`** of a registered Host. If the default host is renamed in the Hosts registry, trigger fails until the default is updated — this is intentional fail-fast behavior.

---

## Compatibility

- New fields are optional; pipelines that omit them validate exactly as before.
- Unknown keys are still rejected by `deny_unknown_fields` on `InputDef` — typos like `requird:` fail YAML validation immediately.
- `PipelineInputDef` (DB `pipelines.input_defs`), `InputData` (Validate/Definition views), and the TypeScript `PipelineInputDef` all carry the new fields end-to-end.

## Tests

- `crates/shared/tests/input_validation_test.rs` — unit tests for every constraint kind, aggregation, and definition errors.
- `crates/shared/tests/pipeline_yaml_test.rs` — YAML lint warning tests for missing / inconsistent `type` declarations (plus existing parse coverage).
- `crates/manager/src/api/tests/input_validation_trigger_tests.rs` — API tests: valid inputs pass, missing-required / pattern / choice / host / number / boolean violations return 422, no job is created on failure, defaults are merged, and the webhook path enforces the same rules.
- `dashboard/src/utils/__tests__/pipelineInputs.test.ts` + `error.test.ts` — form normalization and field-error parsing.
- `dashboard/src/utils/__tests__/pipelineYamlLint.test.ts` — warning positioning: field-scoped warnings land on the input key, global warnings stay at the top.
