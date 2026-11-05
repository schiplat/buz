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

### macOS system-proxy probe stalls the first reqwest client build (2026-11-05)

The first `reqwest::Client::build()` in a process blocks 9–17s on this
Mac: hyper-util's macOS proxy matcher opens a SystemConfiguration store
(`SCDynamicStoreCreateWithOptions` → `_SC_getApplicationBundleID` →
`dispatch_once_callout`), and that one-time init takes 10s+ here —
measured by sampling the blocked thread. Refined by probes: TLS init is
~0.5 ms, `scutil --proxy` shows no proxy configured at all, restarting
configd (`killall configd`) does **not** help, and Python's
`urllib.request.getproxies()` — same SystemConfiguration path from a
system binary — answers in 7 ms. So the SC framework is healthy and
the stall is specific to unregistered dev binaries under
`target/debug/deps/` resolving their bundle ID via LaunchServices —
the same pathology family as the rustc startup hangs above. Repeat
builds are ~100 µs (the matcher caches globally per process), and a
`.no_proxy()` build never pays the tax because it skips the SC store
entirely.

Impact: every manager integration-test binary that saves a pipeline or
syncs plugins builds a client once, so ~25 binaries each ran 10–17s
longer than their test bodies warrant; the full local suite spent
~8.4 of its 8.6 minutes of test execution in binaries >5s, most of it
this tax. CI (Linux) was never affected — Linux proxy detection reads
environment variables only.

Fix: `BUZ_HTTP_NO_SYSTEM_PROXY=1` (set in `.cargo/config.toml [env]`
and in the CI workflow env) makes all manager clients skip the OS
probe and honor proxy environment variables explicitly
(`http_proxy`/`https_proxy`/`all_proxy`/`no_proxy`, parsed in
`http_util::client_builder`). Trade-off: deployments that rely on
OS-level proxy settings (macOS/Windows system proxies) must switch to
environment variables when the flag is set. Linux deployments are
semantically identical either way. HTTP clients are also reused
instead of rebuilt per request (`4a4b1fe`).

## Deferred designs (roadmap, by priority)

### 0. Test-suite speed — measured, closed (2026-11-05)

Baseline (`RUSTFLAGS="-D warnings" cargo test --all`, cold cache):
132 binaries, 995 tests, all green in ~50 min wall — of which only
8.6 min is test execution; the rest is the CI-equivalent full
recompile. Follow-ups decided:

- Template-DB harness **done** (`d994b21`): migrations run once per
  process into a file-backed template, boots clone it in milliseconds
  (17-test `ops_sandbox` binary: 1.21s total).
- Long-poll test aggregation **not needed**: the poll helpers send no
  `Prefer: wait` header, so the runner poll answers immediately; the
  slow binaries were paying the proxy probe above, not long polls.
- Repo split (manager/runner) **not needed**: its motivation was test
  time, which the harness + measurements above resolved.

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
