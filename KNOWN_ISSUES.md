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

### Intermittent cargo/rustc child-process startup hangs (macOS, 2026-11-05)

Beyond the nextest hang above, plain `cargo test` / `cargo fmt` was observed
(2026-11-05) intermittently hanging with child processes stuck at startup:
rustc at 0% CPU with a tiny (~18 MB) footprint for 30+ minutes, never
producing diagnostics. Killing everything (`pkill -9 -f cargo; pkill -9 -f
rustc`) and rerunning the same command completes normally. Two related
quirks: a cargo run killed mid-build can leave an orphan process holding the
`target/debug/.cargo-lock` (later runs then sit at 0% CPU "waiting"), and
running two cargo builds concurrently (e.g. `-p manager` and `-p shared`
tests) serializes on that lock and looks hung. Local practice: run cargo
strictly one at a time, prefer direct file-redirected background runs over
pipes (piped output is swallowed by `tail`/`grep` so a stuck build is
invisible), and check `ps aux | grep cargo` for orphans before assuming a
hang.

## Deferred designs (roadmap, by priority)

### 1. Scheduler starvation control — **done (2.0.3)**

`backfill` now stops overtaking a pending job once it has waited
`scheduler_starvation_seconds` (default 600s, `0` disables). See the
"Starvation aging" note in `sandbox-host-configuration.md`.

### 2. Grants approval flow — **done (2.1.0)**

Grant/revoke now route through the same pending-changes + approval
mechanism as host-config changes, with blast radius (pipelines referencing
the profile, queued jobs affected) shown at approval time.

### 3. Pipeline-scoped resource grants — **recorded, deferred (2026-11-04)**

Grants are team-level ("this team may run `gpu-8x` anywhere"). A later
option is per-pipeline restriction within an authorized team
(`pipeline_resource_grants`, checked after `ensure_team_may_use` at
trigger). Deliberately not scheduled — revisit on demand.

### 4. Pre-provisioned `BUZ_RUNNER_ID` — **done (2.1.0)**

`BUZ_RUNNER_ID` pins a registration to a pre-declared runner row for
IaC / batch registration; the row is claimed only by that secret.
