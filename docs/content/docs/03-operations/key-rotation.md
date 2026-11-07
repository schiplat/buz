---
title: Key Rotation
weight: 70
description: Rotating auth.jwt_secret with in-place re-encryption
---

The Manager's `jwt_secret` is a **root key**: it signs login/runner tokens
and derives the AES-256-GCM key that encrypts secrets at rest (unsalted
SHA-256). Rotating it invalidates every stored ciphertext — unless the
ciphertexts are re-encrypted under the new key at the same time.
`buz-manager rotate-jwt-key` performs exactly that.

## When to rotate

- The current secret may have been exposed (shared logs, backups, team
  churn).
- The instance was upgraded from an old release that accepted weak or
  default secrets (`changeme-demo-only` was the example default before
  v2.1.2 refuses them at startup).
- Periodic hygiene.

## Procedure

**1. Stop the Manager service.** The rotation needs exclusive access to the
database.

**2. Run the plan** (default mode — validates, writes nothing):

```bash
buz-manager rotate-jwt-key
```

Output: the current key fingerprint (non-reversible), the per-table
inventory with plaintext sizes, and the validation result. Everything that
could fail — wrong secret, unknown schema, weak new key — fails here with
zero writes.

**3. Apply** (snapshot + re-encrypt + verify in one transaction):

```bash
buz-manager rotate-jwt-key --apply
```

- Writes a snapshot to `<db>.pre-rotation` before touching anything.
- Re-encrypts: Secrets, GitHub connection tokens, git-hook secrets,
  environment secret overrides, host credentials, and `buzenc:v1:` values
  inside historical job trigger params.
- Roundtrips every new blob under the new key **inside the transaction**
  before committing.
- Stores the generated secret to `<db directory>/.jwt_secret` (mode 0600;
  the data directory is gitignored). Provide your own via
  `--new-secret-file <path>` if your deployment injects it another way.

**4. Start the Manager with the new secret:**

```bash
BUZ_JWT_SECRET="$(cat data/.jwt_secret)" buz-manager
```

or put the value in `manager.toml` under `auth.jwt_secret` — the command
prints the exact launch line for your setup.

## What breaks on purpose

- **Login sessions**: all JWTs were signed by the old key — every user and
  Runner must log in again.
- **Runner register tokens**: re-issue via
  `POST /api/v1/runners/:id/register-token/reissue` and update each
  Runner's `register_token` (or `BUZ_REGISTER_TOKEN`), then restart the
  Runner. Offline runners simply re-register next time they start.

## Notes

- The rotation reads the current secret from the config chain — run the
  command with the same `-c` / environment the service uses, or the old
  key won't resolve and the plan fails loudly before any write.
- Historical job trigger params are rotated too, so old job pages keep
  rendering their masked values.
- A dedicated master key with online data migration is still planned; this
  command is the offline rotation path until then.
