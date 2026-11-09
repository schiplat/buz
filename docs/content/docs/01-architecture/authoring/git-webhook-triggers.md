---
title: Git Webhook Triggers
weight: 70
description: "GitHub/Gitee push → signed webhook → job trigger, normalized INPUT_GIT_*, and Trigger events history"
aliases:
  - /docs/01-architecture/git-webhook-triggers/
---

{{< callout type="note" >}}**Design record.** Core push-trigger flow is shipped; sections marked *planned* are not yet implemented.{{< /callout >}}

## Overview

Buz integrates Git hosting platforms (**GitHub**, **Gitee** first) so that a **push** (and later tag/PR) can start a job on an **existing** pipeline. Checkout/build steps stay in the pipeline definition the user already wrote — **no new YAML `git` / `on.git` fields**.

This is separate from the generic HTTP trigger [`POST /api/v1/hooks/:path`](/docs/01-architecture/platform/api-reference/#webhooks) (flat `inputs` / query). Git platforms send a large signed event payload; Buz uses a dedicated adapter endpoint, verifies a shared secret, normalizes fields into `INPUT_GIT_*`, then reuses the existing job trigger path (`do_trigger`).

| Concern | Where it lives |
|---------|----------------|
| Clone / build / deploy steps | Pipeline YAML or form (unchanged) |
| Webhook URL + Secret/Token | Pipeline UI (generated); paste Secret into GitHub/Gitee |
| Trigger history | Pipeline detail → **Trigger events** (dedicated page) |
| Job logs / status | Existing Jobs / Job detail |

---

## Where is the Secret / Token configured?

**Two places — same value:**

1. **Buz (source of truth)**  
   When the user enables Git webhook for a pipeline (or regenerates credentials), Manager:
   - Generates an opaque `hook_key` (URL path segment)
   - Generates a high-entropy **Secret / Token**
   - Stores the secret **encrypted at rest** (same crypto family as Infrastructure Secrets)
   - Shows the plaintext secret **once** (or on explicit “Reveal / Rotate”) in the Pipeline UI

2. **GitHub / Gitee (platform Webhook settings)**  
   User pastes:
   - **Payload URL** = Buz-generated URL (see below) — **no token in the query string**
   - **Secret** (GitHub) or **密码 / 私钥 / Token** (Gitee Webhook password field) = the same value from Buz

| Platform | Buz stores | User pastes into platform | Verification |
|----------|------------|---------------------------|--------------|
| GitHub | encrypted secret | Webhook → **Secret** | `X-Hub-Signature-256` (HMAC-SHA256 of raw body) |
| Gitee | encrypted secret | Webhook → **密码** (token) | Request header `X-Gitee-Token` (or current official equivalent) must match |

**Do not put the secret in the URL.** URL contains only `provider` + `hook_key`. Secrets in query strings leak via access logs, proxies, and Referer.

```
Buz UI:  Generate → URL + Secret (copy)
                │
                ├─► GitHub/Gitee: Payload URL = URL
                └─► GitHub/Gitee: Secret/密码   = Secret
```

Rotate: Buz “Rotate secret” → user updates the platform Webhook secret to the new value. Old secret immediately invalid.

---

## Endpoint (receive)

```
POST /api/v1/hooks/git/{provider}/{hook_key}
```

- `{provider}`: `github` | `gitee` (later `gitlab`, …)
- `{hook_key}`: opaque id bound to one pipeline’s git-hook config
- Auth: **no JWT**; authenticity = signature / token header vs stored secret
- Body: platform JSON; implementation must verify against **raw body** bytes before parse

| Outcome | HTTP (typical) | `pipeline_trigger_events.decision` |
|---------|----------------|-------------------------------------|
| Bad / missing signature | 401 | `rejected` |
| Unknown hook_key | 404 | _(no row or rejected)_ |
| Filter skip (branch, `[skip ci]`, …) | 202 | `skipped` + `skip_reason` |
| Ping / handshake | 200 | recorded; no job |
| Trigger OK | 200 | `triggered` + `job_id` |

Idempotency (per pipeline):

| Provider | Delivery key |
|----------|----------------|
| GitHub | `X-GitHub-Delivery` (UUID). Fallback: `github:{repo}:{ref}:{sha}` |
| Gitee | **Not** `X-Gitee-Timestamp`. Stable composite: `gitee:{repo}:{ref}:{sha}` |

Before `do_trigger`, Manager **claims** the row with `INSERT OR IGNORE` (`decision=processing`) against the unique `(pipeline_id, delivery_id)` index. Concurrent/retried deliveries see the existing row and do not create a second job. If trigger fails, the claim is **released** so the platform can retry.

---

## Flow

```
GitHub / Gitee Push
    → POST /api/v1/hooks/git/{provider}/{hook_key}
    → load hook config (pipeline, secret, filters)
    → verify signature / token
    → parse → NormalizedGitEvent
    → filters (branch / tag / [skip ci])
    → insert pipeline_trigger_events
    → map inputs → do_trigger(...)
    → update event with job_id / decision
```

Manager does **not** clone the repo. Steps use secrets (deploy key / token) and `$INPUT_GIT_*` as today.

---

## Normalized inputs (`INPUT_*`)

On successful trigger, Manager passes string inputs (exported to the runner as `INPUT_<KEY>`):

| Key | Example | Notes |
|-----|---------|--------|
| `GIT_PROVIDER` | `gitee` | |
| `GIT_EVENT` | `push` / `pull_request` | normalized event kind |
| `GIT_REPO` | `org/myapp` | full name |
| `GIT_CLONE_URL` | `git@gitee.com:org/myapp.git` | when present in payload |
| `GIT_REF` | `refs/heads/main` | base branch for pull requests |
| `GIT_BRANCH` | `main` | branch pushes; base branch for pull requests |
| `GIT_TAG` | `v1.2.0` | tag events |
| `GIT_SHA` | full commit sha | head commit for pull requests |
| `GIT_SHA_SHORT` | 7-char prefix | |
| `GIT_COMMIT_MESSAGE` | first line, truncated | commit subject; PR title for pull requests |
| `GIT_SENDER` | login | |
| `GIT_COMPARE_URL` | compare URL | PR page URL for pull requests |
| `GIT_PR_NUMBER` | `42` | pull requests only |
| `GIT_PR_BASE` | `main` | pull requests only |
| `GIT_PR_HEAD` | `feat/thing` | pull requests only |

Pipeline steps reference these like any other input (e.g. `$INPUT_GIT_SHA`). **YAML does not declare these keys** — they are injected by the adapter when the job is created from a Git event. Manual/cron/generic webhook triggers simply omit them (or leave empty).

### Pull requests

Hooks accept push events by default. A hook created with `event_filter:
push,pull_request` (or just `pull_request`) also fires on GitHub
`pull_request` deliveries and Gitee `Merge Request Hook` deliveries:

- **Actionable actions:** GitHub `opened` / `synchronize` / `reopened` /
  `ready_for_review`; Gitee `open` / `update` / `reopen`. Other actions
  (`closed`, `labeled`, `merged`, …) are recorded as skipped and acknowledged,
  so the provider does not retry.
- **Draft PRs** are skipped until `ready_for_review`.
- **`branch_filter` matches the base branch** (the branch the PR merges into).
- **`[skip ci]`** is read from the PR title / merge-request title.
- Event-filter changes are a delete-and-recreate of the hook (no update API).

Legacy hooks keep their stored default (`push`), so upgrading never starts
unexpected PR builds.

---

## Configuration scope (UI / DB only)

A pipeline may have **multiple** Git webhooks (e.g. one for GitHub, one for Gitee, or different branch filters). Not in YAML — configure under Pipeline → **Trigger events**.

**Immutable after create:** no update/rotate API. To change provider, filter, or secret — **delete** the hook and **create** a new one (new URL + secret).

| Field | Description |
|-------|-------------|
| `label` | Optional display name |
| `enabled` | Set at create (default on) |
| `provider` | `github` \| `gitee` |
| `hook_key` | URL path segment (unique) |
| `secret` | encrypted; plaintext shown once at create |
| `branch_filter` | e.g. `main`, `release/*`; empty = all. For pull requests, matched against the base branch |
| `event_filter` | `push` (default) \| `pull_request` \| `push,pull_request` |
| `environment` | Required at create; must be one of the pipeline's declared `environments`. Every delivery triggered by this hook targets exactly this environment — it cannot be overridden per request. |

Generic `on.webhook` / `webhook_path` remains for human/CI HTTP triggers (its `environment` selector precedence: query > body > YAML default).

---

## Trigger events (Pipeline detail page)

Dedicated menu/page: **Trigger events** (not merged into Jobs).

**Jobs** = executions (logs, duration, re-run).  
**Trigger events** = every receive/decision, including skips and auth failures.

### Columns (list)

| Column | Content |
|--------|---------|
| Time | `received_at` |
| Source | `git` / `webhook` / `manual` / `cron` (+ provider icon when git) |
| Ref | branch or tag + short sha |
| Commit | message one-liner + sender |
| Decision | `triggered` \| `skipped` \| `rejected` \| `failed` |
| Job | status badge + link when `job_id` set |

### Filters

Source, decision, time range.

### Also record non-Git triggers

Manual, cron, and generic `/hooks/:path` writes rows with the appropriate `source` so the timeline is unified.

---

## Data model (planned)

### `pipeline_git_hooks` (or columns on a hook config table)

| Column | Notes |
|--------|-------|
| `id` | PK |
| `pipeline_id` | FK (many hooks per pipeline) |
| `label` | Optional UI name |
| `provider` | `github` \| `gitee` |
| `hook_key` | UNIQUE |
| `secret_ciphertext` | AES-GCM |
| `branch_filter` | TEXT |
| `environment` | Fixed target environment, bound at create; must be declared by the pipeline |
| `enabled` | 0/1 |
| `created_at`, `updated_at` | |

### `pipeline_trigger_events`

| Column | Notes |
|--------|-------|
| `id` | PK |
| `pipeline_id` | FK |
| `source` | `git` \| `webhook` \| `manual` \| `cron` |
| `provider` | nullable |
| `event_type` | `push`, `ping`, … |
| `delivery_id` | idempotency |
| `repo_full_name`, `ref`, `branch`, `tag` | |
| `commit_sha`, `commit_message`, `sender` | |
| `compare_url` | optional |
| `received_at` | |
| `decision` | `triggered` \| `skipped` \| `rejected` \| `failed` |
| `skip_reason` | |
| `job_id` | nullable FK |
| `created_at` | |

Unique: `(pipeline_id, delivery_id)` when `delivery_id` present.

See also [Database Schema](/docs/02-development/database-schema/) (tables added when implemented).

---

## Local verification (no public URL)

When Manager runs on localhost, GitHub/Gitee cannot reach it. Simulate platform deliveries with `curl`. Create a hook in Pipeline detail → **Trigger events**, copy the one-time **Secret** and the `hook_key` from the Payload URL, then substitute the placeholders below.

| Placeholder | Meaning |
|-------------|---------|
| `<HOOK_KEY>` | Opaque path segment from Buz (not a secret, but treat as opaque) |
| `<SECRET>` | Webhook secret shown once at create — **never commit real values** |
| `<PORT>` | Local Manager listen port |

Check Pipeline detail → **Trigger events**: success → `triggered` + `job_id`; wrong secret → `rejected` / `bad_signature`.

### Example A — Gitee (token header)

```bash
# Redacted placeholders only — do not paste real secrets into docs or git.
HOOK_KEY='<HOOK_KEY>'
SECRET='<SECRET>'
PORT='<PORT>'

curl -i -X POST "http://127.0.0.1:${PORT}/api/v1/hooks/git/gitee/${HOOK_KEY}" \
  -H "Content-Type: application/json" \
  -H "X-Gitee-Token: ${SECRET}" \
  -H "X-Gitee-Event: Push Hook" \
  -H "X-Gitee-Timestamp: local-test-0001" \
  -d '{
    "ref": "refs/heads/master",
    "after": "abcdef0123456789abcdef0123456789abcdef01",
    "commits": [{
      "id": "abcdef0123456789abcdef0123456789abcdef01",
      "message": "docs: local webhook smoke test",
      "author": { "name": "dev", "username": "dev" }
    }],
    "repository": {
      "path_with_namespace": "example/demo",
      "git_ssh_url": "git@gitee.com:example/demo.git"
    },
    "sender": { "login": "dev" }
  }'
```

If the hook **Branch filter** is `master`, keep `"ref": "refs/heads/master"`. Empty filter matches any branch.

### Example B — GitHub (HMAC-SHA256 of raw body)

Signature must be computed over the **exact** JSON bytes sent as `-d "$BODY"`.

```bash
HOOK_KEY='<HOOK_KEY>'
SECRET='<SECRET>'
PORT='<PORT>'

BODY='{"ref":"refs/heads/master","after":"abcdef0123456789abcdef0123456789abcdef01","compare":"https://github.com/example/demo/compare/0000000...abcdef0","repository":{"full_name":"example/demo","ssh_url":"git@github.com:example/demo.git"},"head_commit":{"id":"abcdef0123456789abcdef0123456789abcdef01","message":"docs: local webhook smoke test","author":{"username":"dev"}},"sender":{"login":"dev"}}'

SIG="sha256=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$SECRET" | awk '{print $2}')"

curl -i -X POST "http://127.0.0.1:${PORT}/api/v1/hooks/git/github/${HOOK_KEY}" \
  -H "Content-Type: application/json" \
  -H "X-GitHub-Event: push" \
  -H "X-GitHub-Delivery: local-test-0001" \
  -H "X-Hub-Signature-256: ${SIG}" \
  -d "$BODY"
```

**Do not** log or commit `${SECRET}` / real `X-Hub-Signature-256` values. Rotate the hook (delete + recreate) if a secret was exposed.

Optional: expose localhost with a tunnel (`cloudflared` / ngrok) and paste that HTTPS URL into the platform Webhook settings when you need a real push from GitHub/Gitee.

---

## Security

1. Verify signature/token before any trigger side effect  
2. Secret never in URL; never logged in full  
3. Opaque `hook_key`  
4. Optional rate limit per hook (later)  
5. Commit message `[skip ci]` / `[ci skip]` → `skipped`

---

## Phased delivery

| Phase | Scope |
|-------|--------|
| **P0** (**implemented**) | Tables + Trigger events UI; record manual/webhook/cron; GitHub **and** Gitee push verify + `INPUT_GIT_*` + trigger; branch filter; claim-before-trigger idempotency (Gitee composite delivery key); ping |
| **P1** | Path filters; UI polish |
| **P2** | Path filters; PR events; status back to GitHub Checks / Gitee comment |

---

## Out of scope (v1)

- New pipeline YAML git trigger syntax  
- Manager-side `git clone`  
- GitHub App install flow  
- Replacing generic `/api/v1/hooks/:path`

---

## Related

- [API Reference — Webhooks](/docs/01-architecture/platform/api-reference/) (generic + git adapter outline)
- [Pipeline Format](/docs/01-architecture/authoring/pipeline-format/) (no YAML change for git hooks)
- [Secrets Access Control](/docs/01-architecture/platform/secrets-access-control/) (deploy keys for checkout steps)
- [Database Schema](/docs/02-development/database-schema/)
