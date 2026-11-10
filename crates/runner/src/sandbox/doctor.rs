//! `buz-sandboxd doctor` — read-only host readiness report.
//!
//! Answers "which sandboxed tiers can this host deliver, and if not, what is
//! missing?" **before** enrollment: no Manager, token, or applied policy is
//! required. Everything here is a prerequisite check; the authoritative
//! verdict remains the Manager-driven capability probe, which creates,
//! starts, execs into and removes a container with the policy's pinned image
//! (`super::probe_backend`). `--probe-image` offers that end-to-end check
//! from the CLI for operators who want to validate a candidate image before
//! publishing a host document.
//!
//! The tier ladder and prerequisite rules come from [`super::backend`], so
//! doctor and the probe cannot disagree about what a tier needs.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use shared::protocol::RunnerExecutionMode;

/// Host-side inputs doctor checks without any Manager interaction.
#[derive(Debug, Clone, Default)]
pub struct DoctorOptions {
    pub docker_cli: String,
    pub socket: String,
    pub workspace_root: String,
    pub state_dir: String,
    pub token_file: Option<PathBuf>,
    pub allowed_runner_uid: Option<u32>,
    pub manager_url: String,
    pub manager_instance_id: String,
    pub probe_image: Option<String>,
    pub probe_runtime: Option<String>,
}

/// Physical-host facts gathered once and shared by every tier check.
#[derive(Debug, Clone, Serialize)]
pub struct HostReport {
    pub os: String,
    pub arch: String,
    pub kernel: Option<String>,
    /// `1` or `2`; `None` when `/proc/self/cgroup` is unreadable/unrecognised.
    pub cgroup: Option<u8>,
    /// `max_user_namespaces > 0` (Linux); `None` when unknown.
    pub user_namespaces: Option<bool>,
    /// `vmx`/`svm` present in `/proc/cpuinfo` (Linux); `None` when unknown.
    pub virtualization: Option<bool>,
    pub kvm_present: bool,
    pub kvm_readable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DockerReport {
    pub cli: String,
    pub cli_present: bool,
    pub version: Option<String>,
    pub daemon_ok: bool,
    pub runtimes: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BootstrapReport {
    pub socket: String,
    pub workspace_root: String,
    pub workspace_root_present: bool,
    pub state_dir: String,
    pub state_dir_present: bool,
    pub token_file: Option<String>,
    pub token_present: bool,
    /// `None` when no token file is configured (env token cannot be checked).
    pub token_mode_private: Option<bool>,
    pub allowed_runner_uid: Option<u32>,
    pub manager_url_configured: bool,
    pub manager_instance_pinned: bool,
    pub warnings: Vec<String>,
}

/// One tier's prerequisite verdict. `usable` here means "prerequisites look
/// satisfied"; the Manager probe still confirms it with the real image.
#[derive(Debug, Clone, Serialize)]
pub struct TierReport {
    pub tier: String,
    pub usable: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeReport {
    pub image: String,
    pub runtime: Option<String>,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DoctorReport {
    pub host: Option<HostReport>,
    pub docker: Option<DockerReport>,
    pub bootstrap: Option<BootstrapReport>,
    pub tiers: Vec<TierReport>,
    pub probe: Option<ProbeReport>,
}

impl DoctorReport {
    /// `0` = at least one tier's prerequisites look satisfied; `2` = none.
    /// A requested probe that fails is reported as `1` even when a tier looks
    /// usable — the operator asked for that specific image.
    pub fn exit_code(&self) -> i32 {
        if self.probe.as_ref().is_some_and(|probe| !probe.ok) {
            return 1;
        }
        if self.tiers.iter().any(|tier| tier.usable) {
            0
        } else {
            2
        }
    }
}

/// Parsed `/proc/self/cgroup`: `0::…` means unified (v2); three-field lines
/// mean a v1 hierarchy. Hybrid setups report v2 (the tier only cares whether
/// cgroup v2 controllers are available).
pub fn parse_cgroup_version(content: &str) -> Option<u8> {
    if content.lines().any(|line| line.starts_with("0::")) {
        return Some(2);
    }
    if content
        .lines()
        .any(|line| line.split(':').count() >= 3 && !line.trim().is_empty())
    {
        return Some(1);
    }
    None
}

/// Runtime names from `docker info --format '{{json .Runtimes}}'`.
pub fn docker_runtimes(info: &Value) -> Vec<String> {
    let mut names: Vec<String> = info
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// A runtime that can deliver the `vm` tier: Kata's Firecracker variant and
/// other Firecracker-backed OCI runtimes self-identify in their name. The
/// Manager policy still decides which name is actually used; this is only a
/// prerequisite hint.
pub fn find_kvm_runtime(runtimes: &[String]) -> Option<&str> {
    runtimes.iter().map(String::as_str).find(|name| {
        let lower = name.to_ascii_lowercase();
        lower.contains("kata")
            || lower.contains("firecracker")
            || lower == "fc"
            || lower.ends_with("-fc")
    })
}

/// Facts the tier assessment needs, injectable so the matrix is testable on
/// any development machine.
#[derive(Debug, Clone, Default)]
pub struct TierFacts {
    pub os_linux: bool,
    pub docker_ok: bool,
    pub runtimes: Vec<String>,
    pub kvm_readable: bool,
}

/// Prerequisite verdict per sandboxed tier, weakest first. Reasons stay close
/// to the vocabulary the Manager probe reports, so ops sees one story.
pub fn assess_tiers(facts: &TierFacts) -> Vec<TierReport> {
    let has_runtime = |name: &str| facts.runtimes.iter().any(|runtime| runtime == name);

    let mut tiers = Vec::new();

    tiers.push(TierReport {
        tier: RunnerExecutionMode::Container.as_str().to_string(),
        usable: facts.docker_ok,
        reason: if facts.docker_ok {
            "docker daemon reachable".into()
        } else {
            "docker daemon unreachable — check `docker info` and the daemon socket".into()
        },
    });

    let (sandbox_usable, sandbox_reason) = if !facts.os_linux {
        (false, "requires a Linux host".to_string())
    } else if !facts.docker_ok {
        (false, "docker daemon unreachable".to_string())
    } else if has_runtime("runsc") {
        (true, "runsc registered as a Docker runtime".to_string())
    } else {
        (
            false,
            "runsc is not registered as a Docker runtime — install runsc, add it to \
             /etc/docker/daemon.json, restart Docker"
                .to_string(),
        )
    };
    tiers.push(TierReport {
        tier: RunnerExecutionMode::Sandbox.as_str().to_string(),
        usable: sandbox_usable,
        reason: sandbox_reason,
    });

    let (vm_usable, vm_reason) = if !facts.os_linux {
        (false, "requires a Linux host".to_string())
    } else if !facts.docker_ok {
        (false, "docker daemon unreachable".to_string())
    } else if !facts.kvm_readable {
        (
            false,
            "/dev/kvm missing or not readable by this user — expose the device to the \
             sandboxd process (Compose: docker-compose.vm.yml)"
                .to_string(),
        )
    } else {
        match find_kvm_runtime(&facts.runtimes) {
            Some(runtime) => (
                true,
                format!("{runtime} registered as a Firecracker-compatible Docker runtime"),
            ),
            None => (
                false,
                "no Firecracker-compatible runtime registered (expected e.g. kata-fc)".to_string(),
            ),
        }
    };
    tiers.push(TierReport {
        tier: RunnerExecutionMode::Vm.as_str().to_string(),
        usable: vm_usable,
        reason: vm_reason,
    });

    tiers
}

/// Unprivileged token files must not be readable by group/others.
pub fn mode_is_private(mode: u32) -> bool {
    mode & 0o077 == 0
}

/// The probe mirrors `probe_backend`'s fail-closed rule: a mutable tag would
/// make the result meaningless (the operator would test a different image
/// later).
pub fn probe_image_is_pinned(image: &str) -> bool {
    image.contains("@sha256:")
}

async fn command_output(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(format!("cannot run {program}: {error}")),
        Err(_) => Err(format!(
            "{program} did not answer within {}s",
            timeout.as_secs()
        )),
    }
}

fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|content| content.trim().to_string())
        .filter(|content| !content.is_empty())
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|meta| meta.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Option<u32> {
    None
}

fn detect_host_facts() -> HostReport {
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let linux = os == "linux";
    let kvm = Path::new("/dev/kvm");
    let cpuinfo = if linux {
        std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default()
    } else {
        String::new()
    };
    HostReport {
        kernel: if linux {
            read_trimmed("/proc/sys/kernel/osrelease")
        } else {
            None
        },
        cgroup: if linux {
            std::fs::read_to_string("/proc/self/cgroup")
                .ok()
                .and_then(|content| parse_cgroup_version(&content))
        } else {
            None
        },
        user_namespaces: if linux {
            read_trimmed("/proc/sys/user/max_user_namespaces")
                .and_then(|value| value.parse::<u64>().ok())
                .map(|max| max > 0)
        } else {
            None
        },
        virtualization: if linux {
            Some(cpuinfo.contains(" vmx") || cpuinfo.contains(" svm"))
        } else {
            None
        },
        kvm_present: kvm.exists(),
        kvm_readable: std::fs::File::open(kvm).is_ok(),
        os,
        arch,
    }
}

async fn detect_docker(cli: &str) -> DockerReport {
    let cli_present = Path::new(cli).exists();
    let version = command_output(cli, &["--version"], Duration::from_secs(10))
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());

    let mut report = DockerReport {
        cli: cli.to_string(),
        cli_present,
        version,
        daemon_ok: false,
        runtimes: Vec::new(),
        error: None,
    };

    match command_output(
        cli,
        &["info", "--format", "{{json .Runtimes}}"],
        Duration::from_secs(15),
    )
    .await
    {
        Ok(output) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
            match serde_json::from_str::<Value>(&raw) {
                Ok(info) => {
                    report.runtimes = docker_runtimes(&info);
                    report.daemon_ok = true;
                }
                Err(error) => {
                    report.error = Some(format!("cannot parse docker runtimes: {error}"));
                }
            }
        }
        Ok(output) => {
            report.error = Some(
                String::from_utf8_lossy(&output.stderr)
                    .trim()
                    .chars()
                    .take(300)
                    .collect(),
            );
        }
        Err(error) => report.error = Some(error),
    }
    report
}

fn detect_bootstrap(options: &DoctorOptions) -> BootstrapReport {
    let mut warnings = Vec::new();

    let token_present;
    let token_mode_private;
    match options.token_file.as_deref() {
        Some(path) => {
            let present = path.exists();
            token_present = present;
            token_mode_private = file_mode(path).map(mode_is_private);
            if !present {
                warnings.push(format!(
                    "token file {} does not exist — enrollment has not been stored",
                    path.display()
                ));
            } else if token_mode_private == Some(false) {
                warnings.push(format!(
                    "token file {} is readable by group/others — chmod 0400",
                    path.display()
                ));
            }
        }
        None => {
            token_present = false;
            token_mode_private = None;
            if !options.manager_url.trim().is_empty() {
                warnings.push(
                    "a Manager URL is configured but no token file is set — sandboxd will refuse \
                     to start"
                        .into(),
                );
            }
        }
    }

    if options.allowed_runner_uid.is_none() {
        warnings.push(
            "BUZ_SANDBOXD_ALLOWED_RUNNER_UID is unset — sandboxd will refuse to start (set it to \
             the Runner's UID)"
                .into(),
        );
    }
    if !options.manager_url.trim().is_empty() && options.manager_instance_id.trim().is_empty() {
        warnings.push(
            "BUZ_MANAGER_INSTANCE_ID is not pinned — any Manager presenting a valid token would \
             be trusted"
                .into(),
        );
    }

    let workspace_root_present = Path::new(&options.workspace_root).exists();
    if !workspace_root_present {
        warnings.push(format!(
            "workspace root {} does not exist yet — sandboxd creates it root-owned on first start",
            options.workspace_root
        ));
    }
    let state_dir_present = Path::new(&options.state_dir).exists();
    if !state_dir_present {
        warnings.push(format!(
            "state dir {} does not exist yet — sandboxd creates it on first start",
            options.state_dir
        ));
    }

    BootstrapReport {
        socket: options.socket.clone(),
        workspace_root: options.workspace_root.clone(),
        workspace_root_present,
        state_dir: options.state_dir.clone(),
        state_dir_present,
        token_file: options
            .token_file
            .as_ref()
            .map(|path| path.display().to_string()),
        token_present,
        token_mode_private,
        allowed_runner_uid: options.allowed_runner_uid,
        manager_url_configured: !options.manager_url.trim().is_empty(),
        manager_instance_pinned: !options.manager_instance_id.trim().is_empty(),
        warnings,
    }
}

/// End-to-end check of one candidate image, mirroring `probe_backend`:
/// create → start → exec `command -v timeout` → remove. The container is
/// always removed, including on failure.
pub async fn probe_candidate(options: &DoctorOptions) -> ProbeReport {
    let image = options.probe_image.clone().unwrap_or_default();
    let runtime = options
        .probe_runtime
        .clone()
        .filter(|runtime| !runtime.trim().is_empty());
    let mut report = ProbeReport {
        image: image.clone(),
        runtime: runtime.clone(),
        ok: false,
        detail: String::new(),
    };
    if !probe_image_is_pinned(&image) {
        report.detail = "probe image must be pinned by @sha256: digest".into();
        return report;
    }

    let name = format!("buz-sandboxd-doctor-{}", uuid::Uuid::new_v4());
    let cli = options.docker_cli.as_str();

    let mut create_args: Vec<&str> = vec!["create", "--name", &name];
    if let Some(runtime) = runtime.as_deref() {
        create_args.push("--runtime");
        create_args.push(runtime);
    }
    create_args.push(&image);
    create_args.push("sleep");
    create_args.push("30");

    let step = async {
        let output = command_output(cli, &create_args, Duration::from_secs(60))
            .await
            .map_err(|error| format!("create: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "create failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let output = command_output(cli, &["start", &name], Duration::from_secs(30))
            .await
            .map_err(|error| format!("start: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "start failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let output = command_output(
            cli,
            &["exec", &name, "sh", "-c", "command -v timeout >/dev/null"],
            Duration::from_secs(15),
        )
        .await
        .map_err(|error| format!("exec: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "image must provide `sh` and `timeout`: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
    .await;

    let _ = command_output(cli, &["rm", "-f", &name], Duration::from_secs(30)).await;

    match step {
        Ok(()) => {
            report.ok = true;
            report.detail = "container created, started, exec'd and removed".into();
        }
        Err(detail) => report.detail = detail,
    }
    report
}

pub async fn collect(options: &DoctorOptions) -> DoctorReport {
    let host = detect_host_facts();
    let docker = detect_docker(&options.docker_cli).await;
    let bootstrap = detect_bootstrap(options);
    let tiers = assess_tiers(&TierFacts {
        os_linux: host.os == "linux",
        docker_ok: docker.daemon_ok,
        runtimes: docker.runtimes.clone(),
        kvm_readable: host.kvm_readable,
    });
    let probe = if options.probe_image.is_some() {
        Some(probe_candidate(options).await)
    } else {
        None
    };
    DoctorReport {
        host: Some(host),
        docker: Some(docker),
        bootstrap: Some(bootstrap),
        tiers,
        probe,
    }
}

fn print_human(report: &DoctorReport) {
    if let Some(host) = &report.host {
        println!("host");
        println!(
            "  platform        {}/{} (kernel {})",
            host.os,
            host.arch,
            host.kernel.as_deref().unwrap_or("unknown")
        );
        let cgroup = host
            .cgroup
            .map(|version| format!("v{version}"))
            .unwrap_or_else(|| "unknown".into());
        let userns = match host.user_namespaces {
            Some(true) => "enabled",
            Some(false) => "disabled",
            None => "unknown",
        };
        println!("  cgroup          {cgroup}  user namespaces: {userns}");
        let kvm = match (host.kvm_present, host.kvm_readable) {
            (true, true) => "present, readable",
            (true, false) => "present, NOT readable",
            (false, _) => "absent",
        };
        println!("  /dev/kvm        {kvm}");
    }
    if let Some(docker) = &report.docker {
        println!("docker");
        println!(
            "  cli             {} ({})",
            docker.cli,
            docker.version.as_deref().unwrap_or(if docker.cli_present {
                "present"
            } else {
                "missing"
            })
        );
        if docker.daemon_ok {
            println!("  daemon          reachable");
            println!(
                "  runtimes        {}",
                if docker.runtimes.is_empty() {
                    "none reported".to_string()
                } else {
                    docker.runtimes.join(", ")
                }
            );
        } else {
            println!(
                "  daemon          unreachable{}",
                docker
                    .error
                    .as_deref()
                    .map(|error| format!(" ({error})"))
                    .unwrap_or_default()
            );
        }
    }
    if let Some(bootstrap) = &report.bootstrap {
        println!("bootstrap");
        println!("  socket          {}", bootstrap.socket);
        println!(
            "  workspace root  {} ({})",
            bootstrap.workspace_root,
            if bootstrap.workspace_root_present {
                "present"
            } else {
                "missing"
            }
        );
        println!(
            "  state dir       {} ({})",
            bootstrap.state_dir,
            if bootstrap.state_dir_present {
                "present"
            } else {
                "missing"
            }
        );
        match (&bootstrap.token_file, bootstrap.token_present) {
            (Some(path), true) => println!(
                "  token file      {path} ({})",
                match bootstrap.token_mode_private {
                    Some(true) => "0400-style, ok",
                    Some(false) => "readable by group/others — chmod 0400",
                    None => "mode unknown",
                }
            ),
            (Some(path), false) => println!("  token file      {path} (missing)"),
            (None, _) => println!("  token file      not configured"),
        }
        println!(
            "  manager         {}{}",
            if bootstrap.manager_url_configured {
                "configured"
            } else {
                "not configured (offline: cache only)"
            },
            if bootstrap.manager_instance_pinned {
                ", instance pinned"
            } else {
                ""
            }
        );
    }

    println!();
    println!("{:<10} {:<8} why", "tier", "status");
    for tier in &report.tiers {
        println!(
            "{:<10} {:<8} {}",
            tier.tier,
            if tier.usable { "usable" } else { "blocked" },
            tier.reason
        );
    }

    if let Some(probe) = &report.probe {
        println!();
        println!(
            "probe           {} ({}) — {}",
            if probe.ok { "PASS" } else { "FAIL" },
            probe.runtime.as_deref().unwrap_or("default runtime"),
            probe.detail
        );
    }

    println!();
    println!(
        "note: tier images and runtimes are Manager policy. Prerequisites above are necessary, \
         not sufficient; publishing a host document enables a tier, and the Manager probe \
         confirms it with the real image."
    );

    if let Some(bootstrap) = &report.bootstrap {
        if !bootstrap.warnings.is_empty() {
            println!();
            for warning in &bootstrap.warnings {
                println!("warning: {warning}");
            }
        }
    }
}

/// Print the report (human or JSON) and return the process exit code.
pub async fn run(options: &DoctorOptions, json: bool) -> i32 {
    let report = collect(options).await;
    let code = report.exit_code();
    if json {
        match serde_json::to_string_pretty(&serde_json::json!({
            "ok": code == 0,
            "host": &report.host,
            "docker": &report.docker,
            "bootstrap": &report.bootstrap,
            "tiers": &report.tiers,
            "probe": &report.probe,
        })) {
            Ok(rendered) => println!("{rendered}"),
            Err(error) => {
                eprintln!("cannot render report: {error}");
                return 1;
            }
        }
    } else {
        print_human(&report);
    }
    code
}
