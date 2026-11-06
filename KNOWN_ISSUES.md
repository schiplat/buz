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

### Intermittent child-process startup hangs — dyld confirmed (macOS, 2026-11-05/06)

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

2026-11-06 update — root cause identified by sampling during a hang
window: the stuck child sits at `_dyld_start` (the macOS dynamic
linker entry) indefinitely, 0% CPU, before any of its own code runs.
It hits rustc invocations, test binaries, and cargo itself at random;
a `manager` test run needed a watchdog wrapper (kill + restart on 120s
of zero CPU-time progress) to get through — ten consecutive attempts
hung before a healthy window appeared, then a full pass completed
(manager 2350/0). The host had 68 days uptime when first observed;
a reboot is the likely reset for whatever degrades dyld's startup
path. During a hang window nothing local helps — detect and retry.

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

### 0a. Kernel retirement — **done (v3.0.0)**

Phase 3 of `registry-auth-dual-track.md` / `object-store-config-source.md`
shipped: `registry_secret`, `config.source`/`config.key_path` (OSS
deliver), the secret kinds `registry`/`aliyun_oss_auth`, the Manager
signed-URL attachment, and the clock-skew guards are removed. YAML
`deny_unknown_fields` rejects the retired keys; `buz-manager
scan-deprecated` reports pre-upgrade usage; DB columns stay (inert,
retained for legacy data). Legacy secret kinds normalize to `generic`.

### 0b. settings_test parallel flake (pre-existing, observed 2026-11-06)

`settings_test::update_persists_applies_and_reset_falls_back` failed
once inside a full `cargo test --all` run ("override must reach the
resolver") while passing consistently in isolation. `policy_from_settings()`
reads process-global state that parallel test binaries in other suites
may reset — a test-isolation defect, not a product bug. Not caused by
the v3 work (plugin/settings code untouched). Fix candidate: make the
resolver policy per-app-state instead of process-global.

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

The two approval switches (`sandbox_config_require_approval` /
`grants_require_approval`) were UI-less and default-off until 2026-11-06,
when a single platform-admin "Require approvals" toggle shipped in the
sandbox-hosts page header. The UI writes both keys together — display is
ON when either key is on, and switching off writes both off (which can
silently overwrite an intentional API-side split; considered acceptable).
The API keeps them independently settable.

### 3. Pipeline-scoped resource grants — **recorded, deferred (2026-11-04)**

Grants are team-level ("this team may run `gpu-8x` anywhere"). A later
option is per-pipeline restriction within an authorized team
(`pipeline_resource_grants`, checked after `ensure_team_may_use` at
trigger). Deliberately not scheduled — revisit on demand.

### 3b. Secrets declaration — GHA-alignment split (2026-11-06)

Findings from migrating the super-manager deploy pipeline to plugin
steps (`buz/config-fetch` / `buz/docker-login` / `buz/docker-logout`):

1. `collect_effective_secret_names` scans top-level/step `secrets:`
   lists, `env:` and `notify` references — **not `uses:` steps' `with:`
   maps**. Until the collector covers `with:`, uses-based pipelines must
   declare those secrets in the top-level `secrets:` list (their only
   bind/validate path), and the "which pipelines use secret X" audit
   misses with:-only references. Fix candidate: scan `with:` values
   (small; blocked only on landing in the same tree as the in-flight
   shared/pipeline_yaml work).
2. Even after 1, the top-level list stays **required** for pipelines
   with `config.render`: render references live in external template
   files (env.example inside the config bundle) that no YAML scan can
   see, so the declaration is the static injection allowlist for
   external templates — a deliberate difference from GHA (which has no
   external-template secret resolution and fails silently on typos;
   buz's `on_missing: fail` fails loudly per reference instead).
3. Long-term (post-v3 candidate): extract render into a `buz/env-render`
   plugin with explicit `with:` secret mapping — references become
   scannable, the `render_env_secrets` side-channel and the `config:`
   core block can be removed. Template-contract route: keep the
   `${secrets.X}` syntax (plugin performs the substitution from the
   mapping) — switching templates to INPUT-style placeholders would
   invalidate every published config bundle on OSS and break
   re-deploy/rollback of historical tags. Requires a digest-pinned
   envtools image (oss-tools precedent) so the dotenv parsing rigor
   (strict key validation, dedup, warn-mode annotations, backup
   rotation) is not reimplemented in shell.

### 4. Pre-provisioned `BUZ_RUNNER_ID` — **done (2.1.0)**

`BUZ_RUNNER_ID` pins a registration to a pre-declared runner row for
IaC / batch registration; the row is claimed only by that secret.
