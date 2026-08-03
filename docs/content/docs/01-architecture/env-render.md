---
title: Env Render — Declarative .env Generation
weight: 5
description: "Render env.example into .env on the target host with explicit ${secrets.X} / ${vars.X} namespaces, GH-Actions-style references, fail-fast on missing sources"
---

> **Status: IMPLEMENTED — Phase 1 engine + Phase 2b authoring shipped.**  
> This page is the **render engine** design: template syntax, `ValueMap`,
> security gates, change detection, flock.  
> Nested step YAML / OSS deliver / render-only / multi-file →
> [Config Block DSL v2](/docs/01-architecture/config-block/) (**2b complete**).
> Auto `on_change`/`services` still deferred (**2d**).  
> All `${secrets.X}` / `${vars.X}` / `$INPUT_*` syntaxes →
> [Secrets & Variables Expressions](/docs/01-architecture/secrets-vars-expressions/).

## Problem

Today a deploy pipeline hand-writes `.env` with shell `upsert_env()` snippets:
verbose, error-prone (quoting / `sed` escapes), and unable to know *whether a
value changed* (so a rotated secret silently doesn't take effect until the
container is recreated).

The desired end state: **the template is the contract.** A pipeline declares a
template (`env.example`) and an output (`.env`); Buz resolves every reference
from the job's visible secrets / pipeline vars and writes the file idempotently.

## Design decisions (agreed)

1. **Template = contract, explicit references, no implicit priority.**
   Rejected "secret-hit > env-expansion > default" auto-discovery: it cannot be
   statically reviewed, fails silently when a secret is missing, and reads
   unpredictably. Instead every reference names its source explicitly — this is
   the GitHub Actions `${{ secrets.X }}` idea, *without* the org/repo/env
   override chain (which we deliberately do not copy).
2. **Two namespaces only** — `${secrets.X}` and `${vars.X}`. No recursive
   expansion, no host-environment fallback.
3. **Fail fast** — a template referencing an undefined secret/vars key fails the
   render (and the job), never silently leaves a blank value.
4. **Runner renders locally on the target** — the template lives on the target
   (unpacked from OSS), and the secrets values are already dispatched to the
   Runner as env vars; rendering writes the same values to a file and adds **no
   new exposure surface**.
5. **Change detection → recreate** — the renderer diffs against the old `.env`
   and reports changed keys so the pipeline can recreate the affected container
   (compose does not restart on `.env` edits by itself).
6. **Secret `name` = the env key = the template key (single namespace).**
   Secret names are enforced **UPPER_SNAKE_CASE** at create — the same name is
   the injected env var (`$POSTGRES_PASSWORD`), the YAML binding
   (`secrets: [POSTGRES_PASSWORD]`) and the `${secrets.POSTGRES_PASSWORD}`
   template reference. Matching is **case-sensitive**; a legacy `env_name`
   column from an earlier draft was merged back into `name` at migration.

   **Upgrade note (`env_name` → `name`):** on Manager start the migration
   backfills `name = env_name` when they differ and the new name does not
   collide with another secret. Colliding rows keep the old `name`, emit a
   `tracing::warn!`, and then lose `env_name` when the column is dropped —
   operators must **delete + recreate** the secret under the intended
   UPPER_SNAKE_CASE name (the Update API cannot rename) and update pipeline
   refs. Legacy non-UPPER names that never had a distinct `env_name` also
   keep working for lookups but cannot be created anew.
7. **OSS deliver unpacks the archive itself; render runs in prepare before the
   run block.** (Review R2 #1 — chosen route A.) Preferred authoring splits
   deliver and render-only steps under nested `config:`. Sequencing:
   download → unpack → [`config.render`] → run block. Prefer a separate
   render-only step after deliver. `$BUZ_CONFIG_DIR` is the working directory
   (`config.dest`).

---

## Template syntax

```env
# ── ${secrets.X} — resolved from Buz Secrets by exact NAME (UPPER_SNAKE_CASE) ──
# Missing → render FAILS (fail fast; prevents a silently-disabled feature).
MANAGER_AFDIAN_API_TOKEN=${secrets.AFDIAN_API_TOKEN}
MANAGER_TURNSTILE_SECRET=${secrets.TURNSTILE_SECRET}
MANAGER_SMTP_USERNAME=${secrets.SMTP_USERNAME}
MANAGER_SMTP_PASSWORD=${secrets.SMTP_PASSWORD}

# ── ${vars.X} — pipeline env: + trigger inputs (INPUT_TAG) ──
# Missing → render FAILS. An inline `:-default` fallback is allowed.
MANAGER_IMAGE=${vars.REGISTRY}/${vars.NAMESPACE}/${vars.IMAGE_NAME}:${vars.INPUT_TAG}
MANAGER_AFDIAN_POLL_INTERVAL_SECS=${vars.AFDIAN_POLL_INTERVAL_SECS:-300}

# ── Bare values — fixed defaults, kept as-is ──
MANAGER_PATH_PREFIX=pi
MANAGER_SESSION_TTL_SECS=43200
```

### Namespace reference

| Namespace | Source | Missing → | Notes |
|-----------|--------|-----------|-------|
| `${secrets.X}` | Buz Secrets, matched by exact secret `name` (UPPER_SNAKE_CASE, case-sensitive), generic kind only | **Render fail** | The `secrets.` prefix doubles as a *sensitivity marker*: sensitive lines are identifiable at a glance in review |
| `${vars.X}` | Pipeline `env:` + trigger inputs (both key forms `NAME` / `INPUT_NAME`) | **Render fail** | `:-default` fallback supported |
| bare value | template itself | — | written as-is |

### Render rules

- **Single pass, no recursion.** Values are substituted once; `$` / `${...}`
  inside a resolved value are NOT re-expanded. (Prevents secret values
  containing `$` from being interpreted.)
- **Residual detection.** After rendering, any remaining `${...}` fails the
  render. Critical: docker-compose interpolates `${VAR}` inside an `env_file`,
  so a leftover template reference would be silently re-interpolated (or
  warned) by compose against host env.
- **Compose-interpolation guard (Review R2 #4).** A resolved value containing
  `${` is rejected at write time — even though single-pass rendering leaves it
  verbatim, compose's env_file interpolation would expand it against host env
  later. The round-trip check simulates compose's interpolation layer (treating
  remaining `${...}` as host-env lookups) so the *post-compose* value is what is
  validated, not just the render output.
- **Key validation.** Keys must match `[A-Za-z_][A-Za-z0-9_]*`; namespaced
  identifiers likewise. Duplicate keys in one template (case-sensitive) → fail.
  Secret names are globally UNIQUE in UPPER_SNAKE_CASE, so a job's env map can
  never collide across secrets (the former `env_name` collision-detection path
  is now structural).
- **Value line-safety.** Resolved values must not contain `\n`; values
  containing `#`, `=`, `"` or leading/trailing whitespace are quoted on write
  (`KEY="value with #"`) so compose's env_file parser keeps them intact.
- **Round-trip check.** After render the file is re-parsed with the same
  compose env_file rules; every key must round-trip to its rendered value or
  the render fails. Guards against parser quirks of the installed compose.
- **Missing references.** `${secrets.X}` / `${vars.X}` not resolvable follow
  `on_missing` (`fail` default, `warn` for rollback). Empty secret values are
  treated as missing. `${secrets.X:?}` marks a hard-required key that fails
  even in `warn` mode.
- **Atomic replace + flock.** Render to a temp file, diff, then atomically move
  into place under `flock` on the output file; a failed render never leaves a
  half-written `.env`, and concurrent renders cannot tear it.
- **Key semantics, not section semantics (Review R2 #5).** The template is the
  source of truth for **every key it defines**, wherever the key sits (above or
  below the `# buz-managed` marker). The marker only preserves keys that the
  template does **not** define (operator-added extras). This fixes the earlier
  self-contradictory "free section is preserved verbatim" wording, which would
  have prevented template updates from ever reaching `.env`.
- **Job-scoped resolution.** Rendering resolves against the secrets already
  dispatched for the job (post-RBAC) and the job's `env` map (with `masking`
  trigger inputs excluded from the vars domain). The Runner never queries the
  database.
- **No value logging (Review R2 #9).** Renderer logs contain key names and
  status only (`resolved` / `missing` / `changed`), never resolved values.
- **Forced value registration (Review R2 #9).** Every secret value written to
  `.env` is registered for value-based redaction **individually** (not just via
  the env key), bypassing `mask_secrets`' 3-char minimum so short secrets are
  masked too. The plaintext `.env` thus never echoes unredacted in job logs.

---

## Step wiring (see Config Block for YAML)

Prepare runs **before** the run block: delivery (if any) → render → `run`.
`$BUZ_ENV_CHANGED` / `_KEYS` are injected only into the step that rendered.
`on_missing: fail` (default) aborts on unresolved `${secrets.X}` / `${vars.X}`;
`warn` writes empty `KEY=` plus a warning comment (rollback deploys).

Authoring shape, DB columns, and samples →
[Config Block DSL v2](/docs/01-architecture/config-block/).

### Security gate on template references

`${secrets.X}` is resolved **only against secrets already authorized for this
job** — explicitly bound (`secrets:` / Authorized Secrets / `team_shared`) and
same-team. A template may NOT pull in an arbitrary same-team secret it never
bound; doing so fails the render. (Review R2 #2.) This matches masking's
producer rule: only values already dispatched into `secrets{}` are resolvable.

The `vars` domain explicitly **excludes trigger inputs marked `masking: true`**
(Review R2 #3): sensitive trigger values can never be routed into `.env` via
the non-sensitive `vars` namespace. If a template needs a sensitive trigger
value, the pipeline must pass it as a secret.

---

## Unified value resolution (masking-parity architecture)

`env_render` is designed as the same two-sided pattern that already powers log
masking — not as another one-off value mechanism.

### Existing value-processing paths today

| Path | Location | Shape |
|------|----------|-------|
| Log masking | `shared::log_masking` + Runner `SecretCatalog` | **Two-sided**: Manager (producer) registers sensitive values with `SecretClass` into `log_mask_values`; Runner (consumer) collects them into `SecretCatalog` and calls `mask_secrets` — zero field knowledge. |
| `config_key_path` templating | `manager::api::runners` + `ValueMap` Lenient | `$tag` / `${tag}` → value; unresolved → empty string. |
| Dynamic host resolution | `manager::api::runners` `resolve_var_ref` | Free function; looks up trigger inputs only. |
| env/secret injection | Dispatch builds `env` + `secrets` maps | Merged at claim; Runner re-merges. |

Masking is the reference abstraction: a **producer/consumer split with a
category registry** — adding a sensitive class means (1) register in
`SecretClass`, (2) register at the Manager production site, (3) **zero Runner
changes**.

### The unified `ValueMap` + expander

"Where does a value come from" and "which values are sensitive" are two
projections of the **same ValueMap**, so they share one source and one consumer:

```rust
// shared::value_source — sits next to log_masking, same shape
pub enum ValueDomain {
    Secrets,   // ${secrets.X} → sensitive; feeds SecretCatalog
    Vars,      // ${vars.X}    → not sensitive; render/expand only
}

/// Strictness is chosen by the caller, not baked into the expander (Review R2 #7):
/// config_path / host resolution use the legacy lenient mode (unresolved → empty,
/// tolerant), env_render uses the strict mode (unresolved → RenderError, fail-fast).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExpandMode { Lenient, Strict }

pub struct ValueMap {
    secrets: HashMap<String, String>,   // secret name -> value  (Manager-built at dispatch)
    vars:    HashMap<String, String>,   // env key -> value   (pipeline env + trigger inputs)
}

pub fn expand(template: &str, map: &ValueMap, mode: ExpandMode) -> Result<Expanded, RenderError>;
```

**Existing code folded in:**

| Existing | Becomes |
|----------|---------|
| `expand_var_refs` (config_path) | bare `$X` / `${X}` mode of `ValueMap::expand` — backward compatible |
| `resolve_var_ref` (hosts) | a `Vars`-domain lookup on the same expander |
| Runner building `SecretCatalog` by hand | derived from `ValueMap.secrets` (`add_keys` + `add_value`); no separate collection |

**Adding a new value source** mirrors masking exactly: (1) add a `ValueDomain`,
(2) push values into `ValueMap` at the Manager production site, (3) **zero Runner
/ expander changes**. The `ValueMap` is built from the same dispatch payload
(`env` / `secrets`), so the Manager change stays in one place and the "what is
sensitive" decision stays owned by `log_masking` (the catalog is just fed by
`ValueMap.secrets`).

---

## Data flow

```
configs.tar.gz (release)        Buz Secrets DB (live)
  env.example (template)          name + value(encrypted)
         │                               │
         │  signed OSS GET (1h)          │ decrypt @ job claim, dispatched as
         ▼                               ▼   secrets{ name: value }
  prepare: OSS unpack to dest     ──►  Runner builds ValueMap (RBAC-filtered;
  (or templates already at dest)         masking inputs excluded from vars)
         │
         ▼  config.render (before run block; multi-file in order)
   outputs (mode 0600, atomic+flock) ──►  diff old ──► $BUZ_ENV_CHANGED(_KEYS)
                                          │           └─ optional on_change (2d)
                                          └─ key semantics
                                          (values registered for redaction)
```

Step machinery order: **[download → unpack] → render → run block → (optional)
`on_change` recreate (2d, not enforced)**.

---

## Pitfalls & resolutions

Each pitfall below carries an agreed resolution. Where the resolution changes
behavior, the change is already reflected in the DSL / render-rules sections.

| # | Pitfall | Resolution |
|---|---------|------------|
| 1 | **Silent feature-disable** — a missing secret blanking a key (e.g. Afdian token) disables an integration with no error | **Fail-fast render** by default (`on_missing: fail`). The error message names the reference (`${secrets.AFDIAN_API_TOKEN}`) and hints "secret may be missing, renamed, or on another team". |
| 2 | **compose `${VAR}` re-interpolation** — leftover template syntax in `.env` is re-interpolated by compose against host env (silently empty) | **One-pass rendering + residual detection**: any `${...}` left after substitution fails the render. A unit test renders a template containing `$` in a resolved value and asserts the output keeps it verbatim. |
| 3 | **Rollback blocks on fail-fast** — an old `configs.tar.gz` references a secret deleted/renamed since | `on_missing: fail \| warn`, **default `fail`**, `warn` reserved for rollback deploys. In `warn` mode the key is written as `KEY=` (empty) **and** a `# WARNING: ${secrets.X} missing at <ts>` comment is written above it, plus a job-log warning. The operator sees exactly which credential is stale. |
| 4 | **`$` in secret values** — a token containing `$` can be re-interpreted on write / by compose | Renderer never re-expands resolved values (single pass). A bare `$` / `$$` keeps verbatim (compose does not expand bare `$`); a value containing **`${` is rejected at write time** (compose would interpolate it — Review R2 #4). Round-trip tests cover `$` / `$$` / `${}` in secret values. Documented in the secrets guide: prefer tokens without `$` when the target app allows it. |
| 5 | **compose env_file parser limits** — `#` / trailing spaces truncate values | **Value line-safety**: values containing `#`, leading/trailing space, `=` or `"` are written quoted (`KEY="…"`). A **round-trip check** runs after render: the renderer re-parses the written file with the same compose-env_file rules and asserts every key round-trips; mismatch fails the render. Quoting rules are validated against the installed compose version in CI. |
| 6 | **Concurrent renders** — two jobs rendering the same `.env` race | Atomic temp-file+rename prevents torn writes. The diff-and-swap critical section runs under **`flock` on the output file** (Linux native, target is Linux). Deploy pipelines are additionally documented as single-flight. |
| 7 | **Overwriting manual edits** — operators tweak `.env` by hand | **Key semantics** (Review R2 #5): the template is the source of truth for every key it defines, wherever it sits; the `# buz-managed` marker only preserves keys the template does **not** define (operator-added extras). `on_change` detection covers template-defined keys only, so operator-only keys never trigger recreates. |
| 8 | **`on_change` scope** — `.env` feeds several services (postgres `POSTGRES_*`, manager-server) | `services:` is explicit in the DSL; the renderer emits `BUZ_ENV_CHANGED_KEYS` so the run block (or `on_change`) recreates **only** the affected service. Default: no auto-recreate; the pipeline author opts in. |
| 9 | **Secret-name churn** — renaming a secret silently orphans templates/scripts | Phase 3 reference tracking (reverse lookup across `pipeline_secrets`, step `secrets:`, `registry_secret`, `config_source`, and render templates). Until then: rename shows a **confirm dialog** in the UI, and any template referencing the old name hits fail-fast (#3) — the reference never fails silently. |
| 10 | **Plaintext secrets on disk** — `.env` holds secret values (vs. host-env passthrough) | Accepted trade-off for the "write `.env`" route. Mitigations: file `0600` + root ownership; `cat .env` output stays value-redacted in job logs; target hardening note (don't ship general-purpose log agents on the deploy host). A **host-env passthrough** route (compose `environment:` + `${VAR:-}`) remains available as the zero-disk option for values that only need in-container visibility. |
| 11 | **Empty-value secrets** — an existing-but-empty secret tripping compose `${VAR:?}` | Empty value is treated as **missing** (goes through `on_missing`, never written as bare `KEY=`). Template may additionally use `${secrets.X:?}` for a hard-required key — fail-fast even in `warn` mode. |
| 12 | **Template packaging gaps** — `configs.tar.gz` missing `env.example` / wrong path | Render fails with an explicit error naming the expected path and the archive it came from. Never a no-op. |

---

## Review Round 2 — vulnerabilities found & resolutions

A second design review surfaced deeper issues; all are resolved below and
already reflected in the DSL / render-rules / ValueMap sections above.

| # | Vulnerability | Resolution |
|---|---------------|------------|
| 1 | **Render timing depends on unpack** — the template arrives inside `configs.tar.gz`, but unpacking happens *in the run block*; a step-level `env_render` has no well-defined insertion point (before unpack it fails, after unpack it's too late for compose up) | **Route A chosen: `config_source` unpacks the archive itself**; render runs immediately after unpack and before the run block. Run block starts with the bundle unpacked (`$BUZ_CONFIG_DIR`) and `.env` already written. Sequencing: download → unpack → render → run → `on_change` recreate. |
| 2 | **Template references bypass Authorized Secrets** — `${secrets.X}` looked up by name/team would let any same-team pipeline pull any same-team secret into `.env` without binding it | Render resolves **only against secrets already authorized for this job** (bound `secrets:` / Authorized Secrets / `team_shared`, same-team). Referencing anything else fails the render — same producer rule as masking. |
| 3 | **`masking: true` trigger inputs leak via `${vars.X}`** — sensitive trigger values would enter `.env` plaintext through the non-sensitive vars domain | `vars` domain **excludes masking inputs**. A pipeline needing a sensitive trigger value in `.env` must route it through a secret. |
| 4 | **`${...}` inside a secret value is re-interpolated by compose** — single-pass rendering leaves it verbatim, but compose's env_file interpolation expands it against host env | **Compose-interpolation guard**: values containing `${` are rejected at write; the round-trip check simulates compose's interpolation layer so the *post-compose* value is validated, not just the render output. |
| 5 | **Free/managed section contradiction** — "template lines above `# buz-managed` are preserved verbatim" would freeze template updates (e.g. a new `MANAGER_SMTP_PORT` never reaches `.env`) | **Key semantics**: the template is the source of truth for every key it defines, anywhere in the file; the marker only preserves keys the template does **not** define (operator-added extras). |
| 6 | **Env-key collisions undetected** — two secrets resolving to one env var bound to one job silently overwrite in the secrets map | Secret names are globally UNIQUE and enforced UPPER_SNAKE_CASE at create, so the inject key and the template key are the same name — a collision is structurally impossible. (The former `env_name` alias column and its per-team uniqueness check were removed.) |
| 7 | **Unified expander would break `config.key_path`** — path expansion is lenient (unresolved → empty), `config.render` must be strict (fail-fast); one expander can't serve both without a mode | `ExpandMode { Lenient, Strict }` — caller chooses. `config_key_path` / host resolution stay lenient; `config.render` uses strict. |
| 8 | **Two-name cognitive load** — template key (left of `=`) and reference (right) could diverge | Resolved by the **single-name merge**: the secret `name` IS the env key, the YAML binding and the template reference — one UPPER_SNAKE_CASE name everywhere, so there is nothing to map. |
| 9 | **Render logs / short values** — renderer logging values leaks secrets; `mask_secrets` skips <3-char values so a short secret in `.env` would echo unredacted | Renderer logs **key names and status only** (`resolved` / `missing` / `changed`), never values. Every secret value written to `.env` is **registered for value-redaction individually** (bypassing the 3-char minimum). |

---

## Compatibility

- Existing secret delivery (`secrets:` list, `team_shared` auto-inject, step
  `secrets:`, `registry_secret`, `config.source`) is unchanged. Rendering is an
  **additive** capability; the same secret can be injected as env **and**
  referenced by a template.
- OSS deliver (`config.source` + `key_path` + `dest`) unpacks to
  `$BUZ_CONFIG_DIR`; without `config.render` it is download → unpack only.
  Render-only steps reuse `dest` without a second OSS download.
- Secret names are UPPER_SNAKE_CASE (enforced at create, exact/case-sensitive
  matching). A legacy `env_name` column from the draft design was **merged back
  into `name`** via migration (backfill env_name → name when non-empty and
  collision-free), then dropped; existing pipelines that referenced the old
  env_name key must re-point at the merged name.
- `registry` / `aliyun_oss_auth` secrets never participate in env rendering
  (they are JSON handled by `registry_secret` / `config.source`).
- The unified `ValueMap::expand` keeps `ExpandMode::Lenient` for
  `config_key_path` / host resolution; `config.render` uses `Strict`.

---

## Phasing (engine)

| Phase | Scope | Status |
|-------|-------|--------|
| 1 | Secret naming; unpack + render; `ValueMap`; fail-fast; `BUZ_ENV_CHANGED`; flock; tests | **Shipped** |
| 2 | `on_missing: warn`; key semantics; **`on_change` / `services` auto-recreate** | **Partly shipped** — warn + change env done; auto-recreate **not** enforced (branch in `run` on `$BUZ_ENV_CHANGED`) |
| 2b | Nested `config:`; multi-file render; OSS deliver + render-only; DB cut-over; UI | **Shipped** — see [Config Block](/docs/01-architecture/config-block/) |
| 2c | `config.git` | Planned |
| 2d | Enforce `on_change` / `services` | Planned |

## Related

- [Config Block DSL v2](/docs/01-architecture/config-block/) — step `config:` (shipped)
- [Secrets Access Control](/docs/01-architecture/secrets-access-control/)
- [Pipeline Format](/docs/01-architecture/pipeline-format/)
