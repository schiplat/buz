# Known Issues & Deferred Work

Known limitations, deferred designs, and tooling quirks. Items here are
deliberate decisions with context, not bugs to fix silently.

## Tooling

### Rust build cache (Swatinem/rust-cache) removed

One cache entry (~500 MB) was measured against the org free-plan storage
quota (500 MB total for artifacts + packages + cache) and dropped
(`b213018`). Revisit when the repo moves to a paid plan or the quota
changes; until then CI pays full cold-compile time (~10 min release,
~8 min test). The Rust toolchain itself is pinned via `rust-toolchain.toml`.

### cargo-nextest hangs on macOS dev machines

`cargo nextest run` stalls at the test binaries' `--list` enumeration phase
(0% CPU, observed 2026-11-02, even for a 2-test binary). CI runs nextest on
Linux successfully; local gates use `cargo test --all` (see AGENTS.md).
Re-verify after nextest/rustc upgrades.

## Deferred designs (roadmap, by priority)

### 1. Scheduler starvation control — **in progress (this release)**

`backfill` lets small jobs overtake a waiting large one indefinitely;
`fifo` can idle capacity. Design record:
`sandbox-host-configuration.md` ("Aging/starvation control is future work").

### 2. Grants approval flow

Resource-profile grants (`PUT/DELETE /ops/resource-grants`) apply directly;
host-config changes go through pending-changes + approval. Evolution: route
grant/revoke through the same mechanism and show blast radius (pipelines
referencing the profile, queued jobs affected) at approval time.

### 3. Pipeline-scoped resource grants

Grants are team-level ("this team may run `gpu-8x` anywhere"). A later
option is per-pipeline restriction within an authorized team
(`pipeline_resource_grants`, checked after `ensure_team_may_use` at
trigger).

### 4. Pre-provisioned `BUZ_RUNNER_ID`

Runner IDs are Manager-issued. An externally chosen `BUZ_RUNNER_ID` for
IaC / batch registration is a possible later addition (see design record,
"Runner IDs").
