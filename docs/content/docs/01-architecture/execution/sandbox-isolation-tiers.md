---
title: Sandbox Isolation Tiers
weight: 45
description: "Design record: job execution modes as isolation-strength tiers — host, container, sandbox (gVisor), vm (Firecracker) — and the pluggable OCI-runtime backend model behind them"
---

This page records a design decision: Buz job execution modes are ordered by **isolation strength**, not by implementation name. Each tier is a contract a job can rely on; the host operator chooses which OCI runtime delivers each tier; periodic capability probes report what each runner can actually deliver. The model explains the existing `host`/`container`/`vm` modes and adds one tier in the middle.

## Status

| Tier | State |
|------|-------|
| `host`, `container`, `vm` | **Shipped** — see [Runner Policy](/docs/01-architecture/execution/runner-policy/#job-wide-execution-modes), [Configuration](/docs/03-operations/configuration/), and [Security Notes](https://github.com/schiplat/buz/blob/master/SECURITY.md) |
| `sandbox` (userspace-kernel sandbox, delivered by gVisor `runsc`) | **Shipped (code, 2026-10-28).** Protocol, Runner config, `buz-sandboxd` probing, Manager gating, and dashboard selection all accept the tier. **Verified end-to-end on real gVisor** (Debian 13 arm64, no `/dev/kvm`, gVisor release-20261026.0, Docker `runsc` runtime): full capability probe, job create/exec/destroy lifecycle, and a step executing inside the gVisor kernel (`uname` → `4.19.0-gvisor`). The verification also caught and fixed a real portability bug: the step-timeout wrapper now uses busybox-compatible short flags (`timeout -s TERM -k 5`), since busybox applets reject GNU's `--signal`. |

Decision date: 2026-10-28.

> Tier numbering: the L0/L1/L2 labels in shipped docs previously put `vm` at L2. With `sandbox` shipped, the ladder is L0 host, L1 container, L2 sandbox, L3 vm everywhere.

## Binding requirements

Hard requirements from the product decision (2026-10-28). Each traces to a section of this page; the implementation checklist references them.

1. **Runner-autonomous detection.** The Runner — not the operator — discovers which tiers the host can deliver. Every configured backend is probed automatically (startup plus periodic freshness), **independent of the currently selected mode**, so switching modes never waits for a first probe. Results are reported on heartbeats, gate web-side mode selection, and the selected backend is re-checked again at every job start. Adding a backend requires configuration only — no manual registration anywhere else.
2. **Clear source architecture; tests in `tests/`.** All backend knowledge (descriptors, prerequisite rules, probe loop) stays inside the sandbox module; engine/poll keep consuming only mode + handle. **Done (T2, 2026-10-28):** every crate's tests — the tier suites plus all previously inline `#[path]` and `mod tests { }` modules — live under `crates/<crate>/tests/`. The binary-only crates gained thin libraries (`manager`, `plugin-cli` joining `runner`) so their modules are reachable from integration tests; see [Project Structure](/docs/02-development/project-structure/#test-layout).
3. **Decoupled for future VMs.** A new backend integrates as a configuration entry (plus prerequisite flags only if it needs new hardware capabilities). The OCI-runtime delegation is the only integration shape this layer supports (see Non-goals), and the job lifecycle code must remain backend-agnostic.
4. **Analysis before code.** Implementation starts from the touch-point inventory and compatibility matrix recorded here (checklist step 0); each phase lands green before the next begins.
5. **Web-side mode control.** Tier selection lives in Manager UI (dashboard) and is presented as the ordered ladder with explicit per-tier guidance (boundary, cost, availability). The Runner reports support; the user decides which supported tier is the runner's default execution mode — new runners start at `host` and nothing is auto-selected (see Resolved decisions, 2–3).
6. **Docs are part of done.** When a tier stabilizes, the Security Notes (both languages), the Configuration page, and this page's status table are updated in the same effort — never deferred to a follow-up.
7. **The tier architecture diagram is normative** — the diagram in [The tier ladder](#the-tier-ladder) is the reference picture; implementation and docs must match it.

## Resolved decisions

Product answers recorded 2026-10-28; the rest of this page follows them.

1. **Test migration: full — completed.** Every tier-related test moved to the crates' `tests/` directories — the new ones **and** the pre-existing inline `#[path]` modules and `mod tests { }` blocks (107 test files across `shared`, `runner`, `manager`, `plugin-cli`; test count unchanged at 892). Helpers the tests reach are now `pub`; the loopback-HTTP escape in the clock guard became an explicit `#[doc(hidden)]` test hook instead of `cfg!(test)`, so the production guard is strictly https.
2. **Detection, not downgrade.** "Autonomous detection and linkage" means the Runner discovers and reports which tiers the host can deliver; **the user chooses the tier**. No automatic fallback in either direction: an unavailable selected tier rejects jobs (existing fail-closed rule), and a newly supported stronger tier is only surfaced, never auto-applied.
3. **Web control is tiered with explicit guidance; the user sets the default.** The mode selector presents the ladder in order (L0 → L3), each option labelled with its tier, isolation boundary, cost, and live availability. The Runner only reports which tiers the host supports; **which tier becomes the runner's default execution mode is the user's decision**, freely settable per runner among tiers reporting `Supported`-and-fresh; unsupported tiers are disabled with the probe reason. New runners register in `host` as the safe starting point — the system never auto-selects a tier on the user's behalf. Per-runner granularity is kept; per-job minimum-isolation scheduling remains a non-goal.

## The tier ladder

| Tier | Isolation boundary | Delivered by (OCI runtime) | Host prerequisites | Primary risk shape |
|------|--------------------|----------------------------|--------------------|--------------------|
| `host` (L0) | None — steps run as the Runner OS user | — | — | Full host compromise if a job turns hostile |
| `container` (L1) | Hardened container: non-root, read-only root, dropped capabilities, `no-new-privileges`, resource limits — **shares the host kernel** | runc (default; no `--runtime` flag) | Docker daemon | Host-kernel attack surface reachable from the container |
| `sandbox` (L2) | **Job never talks to the host kernel**: a userspace application kernel implements Linux syscalls; filesystem access is mediated by a separate gofer process | gVisor `runsc` | Linux host, `runsc` installed; **no KVM required** | Bugs in the syscall-emulation layer (memory-safe implementation, itself confined by its own sandboxing) |
| `vm` (L3) | Hardware virtualization: job runs in a microVM with its own guest kernel | Firecracker-compatible OCI runtime (e.g. Kata `kata-fc`) | Linux host + `/dev/kvm`; runtime registered with the Docker daemon | Bugs in the VMM's minimal device model |

```
job steps (shell)
   │
   ├── L0 host ────────── no isolation ───────────────────────────► host kernel
   │     every syscall lands on the host kernel
   │
   ├── L1 container ─ [namespaces · cgroups · caps dropped] ──────► host kernel
   │     hardened, but the host kernel still executes every syscall
   │
   ├── L2 sandbox ─── [Sentry userspace kernel] · gofer ──────────► host kernel
   │     the host kernel never sees job syscalls · no KVM required
   │
   └── L3 vm ──────── [guest kernel] · [VMM device model] ────────► host kernel
         hardware-enforced boundary · requires Linux + /dev/kvm
```

**L2 and L3 are different risk shapes, not strictly ordered strength.** L2 removes host-kernel exposure entirely but trusts a userspace kernel implementation; L3 keeps a real kernel for the job but trusts a small VMM. A job that trips over L2's syscall coverage may run fine on L3, and the reverse risk profile also differs (device-model bugs vs syscall-emulation bugs). The tier ladder orders the *boundary between job and host kernel*, not a total security ranking.

## Contract vs implementation

Three concerns stay separated on purpose:

| Concern | Owner | Vocabulary |
|---------|-------|------------|
| Job-facing contract | Platform admin (Runner Policy) | Tier id: `host` \| `container` \| `sandbox` \| `vm` |
| Host configuration | Ops, Manager-managed: platform defaults + per-host document (see [Sandbox host configuration](sandbox-host-configuration.md)) | Which OCI runtime + digest-pinned image delivers each tier |
| Capability report | Runner probes → Manager → UI | Per-tier Supported/Unsupported/Unknown + reason + delivering runtime name (e.g. `sandbox (runsc)`) |

Implementation-named modes would leak host details into job semantics and force renames when implementations change (Kata alone ships QEMU/CLH/Firecracker backends). With tiers, swapping `kata-fc` for another L3 runtime is a host-ops change with zero job-facing impact — the same argument that motivated introducing L2: gVisor is not "a container flavor", it is a distinct boundary that deserves its own rung.

## Backend descriptor model

Each non-host tier is described by one descriptor, and all sandbox code paths (job creation, capability probing, prerequisite validation) consume descriptors instead of per-mode branches:

```
BackendDescriptor {
  image: String        // digest-pinned; must provide sh + timeout
  oci_runtime: String  // empty → no --runtime flag (plain runc)
  requires_linux: bool
  requires_kvm: bool
}
```

- Default mapping: `container` → runc (no extra prerequisites), `sandbox` → `runsc` (Linux only, no KVM), `vm` → Firecracker-compatible runtime (Linux + KVM).
- One generic validator replaces the VM-only prerequisite check: OS/`/dev/kvm`/runtime-match rules apply **only** when the descriptor requires them. Plain container mode keeps working on any Docker host, including Docker Desktop.
- The `--runtime` Docker flag is passed whenever `oci_runtime` is non-empty, so today's container and vm behavior is byte-identical after the refactor.
- The five-minute probe (create → start → exec → remove a short-lived workload per backend) extends to every configured tier; results ride the heartbeat as today, now with per-tier status, reason, and runtime name.

Tier delivery is configured in the Manager-managed host document — the `tiers` map, edited under Platform defaults (fleet-wide) or per host on the Sandbox hosts page — and sandboxd fetches it from the Manager (see [Sandbox host configuration](sandbox-host-configuration.md)). The earlier sketch of `runner.toml` `[sandbox]` keys mirrored by `sandboxd.toml` is retired: `sandboxd.toml` no longer exists (sandboxd bootstraps from env: `BUZ_MANAGER_URL` + `BUZ_SANDBOXD_TOKEN` / `BUZ_SANDBOXD_TOKEN_FILE`), and legacy `[sandbox]` tier keys in `runner.toml` are ignored on load.

```json
{
  "tiers": {
    "container": { "image": "…@sha256:…", "runtime": "" },        // L1 — empty runtime → runc
    "sandbox":   { "image": "…@sha256:…", "runtime": "runsc" },   // L2 — gVisor
    "vm":        { "image": "…@sha256:…", "runtime": "kata-fc" }  // L3 — Firecracker-compatible
  }
}
```

## Fail-closed inheritance

The non-Host fail-closed set — SSH steps, per-step Docker engine, and Runner-host config preparation — is keyed on "the job runs with a sandbox handle", not on which tier delivered it. The `sandbox` tier inherits all of it automatically, including cancellation/teardown semantics and the no-Host-fallback rule.

## Where each tier can run (cloud KVM matrix)

L3 requires `/dev/kvm`, which clouds expose very differently:

| Cloud | L3 on ordinary VMs | Path to KVM |
|-------|--------------------|-------------|
| GCP | Yes (Intel Haswell+, e.g. N1/N2; not E2) | `--enable-nested-virtualization` |
| Azure | Yes (Intel v3+, AMD EPYC v5, Ampere Dpsv5/Dpdsv5) | — |
| AWS | No (8th-gen Intel C8i/M8i/R8i excepted) | `.metal`, or C8i/M8i/R8i |
| Alibaba Cloud | No | Elastic Bare Metal (Shenlong `ebm` families) |
| Tencent Cloud | No (whitelist for some families) | Bare Metal (CPM) / Dedicated Host (CDH) |

Practical mapping:

- **KVM available** (GCP/Azure VMs, AWS metal/8i, domestic bare metal) → run L3 (`vm`).
- **Mainland ordinary VM fleets** (Alibaba/Tencent regular instances expose no VT-x) → L1 (`container`) today; **L2 (`sandbox`) is the designed answer for strong isolation there**, because `runsc` needs no `/dev/kvm` and slots into the same OCI-runtime delegation model.
- Mainland runners must be able to pull the digest-pinned tier images (ACR/TCR or a self-hosted registry mirror); the pinning requirement does not change per tier.

## Compatibility & rollout

- **Shared protocol**: `RunnerExecutionMode` gains a `sandbox` variant (serde `"sandbox"`), ordered `host < container < sandbox < vm`; `RunnerExecutionCapabilities` gains per-tier `sandbox` status/reason plus an optional tier→runtime map for UI display. Capabilities live inside the `system_info` JSON blob — **no DB migration**.
- **Runner ↔ buz-sandboxd wire**: `CreateJob`/`ProbeCapabilities` requests become tier-generic (mode + image + oci_runtime). Runner and sandboxd ship in the same image — **upgrade them together**.
- **Upgrade order: runners before manager.** An old runner cannot deserialize a `sandbox` policy value; a new manager sending it to an old runner breaks heartbeats. Registration is unchanged: new runners always start in `host`.

### Implementation checklist

0. **Pre-code analysis (binding requirement 4):** done — see [Step 0 analysis](#step-0-analysis-pre-code-per-binding-requirement-4); re-verify line anchors before each phase.
1. Shared protocol: `RunnerExecutionMode::Sandbox` + isolation-level accessor; capability fields; round-trip and legacy-JSON compatibility tests.
2. Runner config: `[sandbox]` gains `container_runtime` / `sandbox_image` / `sandbox_runtime`; vm-tier field names unchanged.
3. Runner↔sandboxd wire: tier-generic `CreateJob`/`ProbeCapabilities`; a single per-tier descriptor lookup replaces the mode matches.
4. Sandboxd: generic prerequisite validator (`requires_linux`/`requires_kvm`); `--runtime` from the descriptor; probe loop over **all** configured tiers, independent of the selected mode (binding requirement 1).
5. Manager: accept and validate `sandbox` in execution-mode parsing and transition checks (online + five-minute freshness rule reused).
6. Dashboard: mode type union, capability support row, mode selector option gated by per-tier capability with the probe reason shown when disabled, runtime-name display (binding requirement 5).
7. Tests — in the crates' `tests/` directories per binding requirement 2: supervisor scenarios (runtime flag args, no-KVM prerequisites, probe cleanup), wire format, manager validation, dashboard rendering.
8. Docs — part of done, not a follow-up (binding requirement 6): update [Security Notes](https://github.com/schiplat/buz/blob/master/SECURITY.md) (both languages) and [Configuration](/docs/03-operations/configuration/); flip this page's status table to "shipped".

### Step 0 analysis (pre-code, per binding requirement 4)

Verified against the source tree 2026-10-28. Paths are line-anchored so later phases can diff against them.

**Touch-point inventory**

| Layer | File | What changes |
|-------|------|--------------|
| shared | `crates/shared/src/protocol/mod.rs:197-213` | `RunnerExecutionMode` + `Sandbox` variant, `as_str`, isolation-level accessor |
| shared | `crates/shared/src/protocol/mod.rs:264-287` | `RunnerExecutionCapabilities` + `sandbox`/`sandbox_reason`/`runtimes` (all `#[serde(default)]`) |
| runner | `crates/runner/src/config.rs:26-57` | `SandboxSection` + `container_runtime` / `sandbox_image` / `sandbox_runtime` |
| runner | `crates/runner/src/client/sandbox_client.rs:23-51,112-171,246-261` | `CreateJob`/`ProbeCapabilities` become tier-generic; per-tier descriptor lookup |
| runner | `crates/runner/src/client/heartbeat.rs:20-36,85-86` | probe/report already mode-independent (requirement 1 satisfied by construction); extend to sandbox tier |
| runner | `crates/runner/src/client/poll/mod.rs:366,1797` | fail-closed gates key on "non-Host / handle exists" — no change needed; `:810-813` `$BUZ_ENV` read stays Vm-only initially |
| sandboxd | `crates/runner/src/sandbox/mod.rs:70-97,317-333,335-384,449-539,541-684` | config fields, generic prerequisite validator, descriptor-driven `--runtime`, probe loop over all configured tiers, `"sandbox"` mode parse |
| manager | `crates/manager/src/api/runners.rs:143-149,2616-2651` | `"sandbox"` parse branch + transition validation (same online + 5-min freshness rule) |
| manager | `crates/manager/src/db/mod.rs:147-148` | none — capabilities ride the `system_info` JSON blob |
| dashboard | `dashboard/src/api/runners.ts:12-15,46`; `dashboard/src/pages/Runners.vue:110-132,844-859` | type union, tiered selector with per-tier guidance (boundary/cost/availability), runtime-name display |
| deploy | `deploy/conf/{runner,sandboxd}.toml`, `docker/docker-compose.vm.yml` | new tier keys documented; vm compose override unchanged |

**Upgrade compatibility matrix**

| Manager | Runner | Sandboxd | Result |
|---------|--------|----------|--------|
| old | new | new | OK — old manager never sends `sandbox`; extra capability fields land in opaque `system_info` JSON |
| new | old | old | **Blocked by design** — old runner cannot deserialize a `sandbox` policy value (heartbeat fails); transition validation also shows `Unknown` capability. Enforces the runners-first upgrade order |
| any | new | old | Probe request shape change degrades safely: old sandboxd reports backends `Unsupported` (fail-closed); runner+sandboxd ship in the same image, so this is transient only |

**Test migration inventory (Resolved decision 1)**

Current repo conventions: `crates/shared/tests/` already holds 13 public-API integration tests; runner and manager keep 71 `#[path]` sibling test modules (manager groups many under `src/api/tests/`, `src/services/tests/`) plus ~13 true inline blocks. Migration implications verified:

- Tier-relevant files migrate cleanly or with minimal exposure: `shared/src/protocol/execution_capabilities_tests.rs` (pub types only), `runner/src/config_tests.rs`, `runner/src/policy_tests.rs`, `runner/src/sysinfo_tests.rs`, `runner/src/client/sandbox_tests.rs` (touches **private wire types** `Request`/`Response` — needs a narrow `pub(crate)`→`pub` wire-type exposure or a test-support constructor), manager `src/api/tests/runner_*.rs`.
- Full-repo migration (all 71 `#[path]` modules + inline blocks) mechanically widens the public API of all three crates wherever tests touch private items — an encapsulation cost that conflicts with binding requirement 3 if done carelessly.

**Staged execution (decided 2026-10-28):** Phase T1 migrates the tier-relevant files together with the tier feature (one reviewable diff); Phase T2 completes the full-repo sweep as a dedicated mechanical change after the feature lands, crate by crate (`shared` → `runner` → `manager`), each keeping `cargo test --all` green. "Full migration" is preserved; the feature review is not buried under an 80-file move.

## Measured overhead

Measured 2026-10-28 on the verification host (Debian 13, **ARM64, 2 vCPU, no `/dev/kvm`**, Docker 29.6.1, gVisor release-20261026.0 on the `systrap` platform, digest-pinned busybox, the same hardening flags buz uses). Round counts and interleaving in parentheses; every command's exit status and a sentinel in stdout were checked.

**Lifecycle through the real `buz-sandboxd` Unix socket** (20 rounds, container=runc vs sandbox=runsc, alternating):

| Phase | L1 container | L2 sandbox |
|-------|--------------|------------|
| create_job | 100.5 ms | 95.9 ms |
| exec_step | 18.3 ms | 20.1 ms |
| destroy_job | 90.0 ms | 86.9 ms |
| **total** | **211.7 ms** | **202.8 ms** |

**Raw Docker lifecycle**, same workload via `docker create/start/exec/rm` directly (15 rounds): 194.3 ms (runc) vs 176.3 ms (runsc). **L2 pays no measurable lifecycle penalty** — the spread is inside run-to-run variance, and `start`/`rm` dominate the total (49–55 ms and 82–90 ms). That cost is Docker daemon/containerd/shim and container teardown, attributed to neither runtime.

**Pure runtime startup**, direct OCI `run` of a verified `/bin/true` bundle with no Docker (21 rounds, median): **runc 11.0 ms, runsc 15.1 ms** — about **+4 ms** for bringing up the gVisor platform. Container start is not where the gVisor cost lives.

**In-job cost attribution**, equal work, 5 interleaved rounds (each timing includes one container run):

| Workload | runc | runsc | Ratio |
|----------|------|-------|-------|
| compute-only shell loop | 303.6 ms | 325.2 ms | 1.07× |
| write 8 MB to stdout | 154.9 ms | 215.6 ms | 1.39× |
| 1000 file create/write/delete on the job workspace | 145.0 ms | 272.1 ms | 1.88× |
| 25 MB of read/write syscalls (`/dev/zero` → `/dev/null`) | 142.7 ms | 352.5 ms | 2.47× |
| 300 process spawns (`/bin/true`) | 181.7 ms | 461.0 ms | 2.54× |

**Reading these numbers:** per job, buz creates one sandbox and runs every step inside it, so the create/destroy cost is paid **once per job** (~200 ms) and each step adds only exec (~20 ms) plus its own work. The gVisor tax is **not** startup — it is per-`fork`/`exec` and per-syscall cost inside long-running steps: **~2–2.5× on syscall-heavy workloads**, **~1.9× on metadata-heavy file work**, and **essentially free for pure compute**. Sizing implication: L2 suits build/test steps that do real CPU work and moderate I/O; it is a poor fit for steps that spawn thousands of short-lived processes or do syscall-dominated file churn. Absolute numbers on a 2-vCPU ARM VM without KVM are a lower bound for typical x86 CI hardware, and the ratios are the transferable part.

## Non-goals

- Non-OCI-shaped integrations — direct Firecracker API control, `firecracker-containerd`, embedded VMMs (libkrun-style). They are a different layer, not additional entries in this one.
- Job-declared minimum-isolation scheduling (manager picks runners by tier requirement). Future policy work; the capability report is designed to support it later.
- Renaming the `firecracker_*` configuration fields; they remain the L3 tier keys.

## References

- [Runner Policy — Job-wide execution modes](/docs/01-architecture/execution/runner-policy/#job-wide-execution-modes)
- [Configuration — sandbox and VM options](/docs/03-operations/configuration/)
- [Sandbox Host Configuration & Resource Profiles](/docs/01-architecture/execution/sandbox-host-configuration/) — accepted design for Manager-managed host policy and named resource profiles
- [Security Notes](https://github.com/schiplat/buz/blob/master/SECURITY.md)
- Upstream: [gVisor](https://github.com/google/gvisor), [Firecracker](https://github.com/firecracker-microvm/firecracker)
