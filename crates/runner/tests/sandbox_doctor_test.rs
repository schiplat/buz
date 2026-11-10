//! `buz-sandboxd doctor` — prerequisite matrix and parsing helpers.
//!
//! These pin the read-only diagnosis: which tiers look deliverable, which
//! reason is reported, and how the exit code maps to "at least one tier" /
//! "none" / "the requested probe failed". The authoritative end-to-end check
//! remains the Manager probe (and `--probe-image`, which reuses the same
//! create/start/exec/remove sequence).

use runner::sandbox::doctor::*;
use shared::protocol::RunnerExecutionMode;

#[test]
fn cgroup_v2_is_recognised() {
    assert_eq!(parse_cgroup_version("0::/user.slice\n"), Some(2));
}

#[test]
fn cgroup_v1_is_recognised() {
    assert_eq!(
        parse_cgroup_version("12:cpu,cpuacct:/system.slice\n1:name=systemd:/init.scope\n"),
        Some(1)
    );
}

#[test]
fn hybrid_cgroup_reports_v2() {
    assert_eq!(
        parse_cgroup_version("0::/\n1:name=systemd:/init.scope\n"),
        Some(2)
    );
}

#[test]
fn empty_cgroup_is_unknown() {
    assert_eq!(parse_cgroup_version(""), None);
}

#[test]
fn docker_runtimes_are_sorted_names() {
    let info = serde_json::json!({
        "runsc": { "path": "/usr/local/bin/runsc" },
        "runc": { "path": "runc" }
    });
    assert_eq!(docker_runtimes(&info), vec!["runc", "runsc"]);
    assert!(docker_runtimes(&serde_json::json!(null)).is_empty());
}

#[test]
fn kvm_runtimes_are_recognised_by_name() {
    for name in ["kata-fc", "kata", "firecracker", "fc", "my-fc"] {
        let runtimes = vec!["runc".to_string(), name.to_string()];
        assert_eq!(
            find_kvm_runtime(&runtimes),
            Some(name),
            "runtime {name} should satisfy the vm tier"
        );
    }
    let plain = vec!["runc".to_string(), "runsc".to_string()];
    assert_eq!(find_kvm_runtime(&plain), None);
}

#[test]
fn container_tier_needs_only_the_docker_daemon() {
    let tiers = assess_tiers(&TierFacts {
        os_linux: false, // Docker Desktop hosts still deliver L1
        docker_ok: true,
        runtimes: vec!["runc".into()],
        kvm_readable: false,
    });
    let container = tiers
        .iter()
        .find(|tier| tier.tier == RunnerExecutionMode::Container.as_str())
        .expect("container tier reported");
    assert!(container.usable, "{}", container.reason);
}

#[test]
fn sandbox_tier_requires_linux_and_runsc() {
    let base = TierFacts {
        os_linux: true,
        docker_ok: true,
        runtimes: vec!["runc".into()],
        kvm_readable: false,
    };
    let no_runsc = assess_tiers(&base);
    let sandbox = no_runsc
        .iter()
        .find(|tier| tier.tier == RunnerExecutionMode::Sandbox.as_str())
        .expect("sandbox tier reported");
    assert!(!sandbox.usable);
    assert!(sandbox.reason.contains("runsc"), "{}", sandbox.reason);

    let with_runsc = assess_tiers(&TierFacts {
        runtimes: vec!["runc".into(), "runsc".into()],
        ..base.clone()
    });
    assert!(
        with_runsc
            .iter()
            .find(|tier| tier.tier == RunnerExecutionMode::Sandbox.as_str())
            .expect("sandbox tier reported")
            .usable
    );

    let macos = assess_tiers(&TierFacts {
        os_linux: false,
        runtimes: vec!["runc".into(), "runsc".into()],
        ..base
    });
    let sandbox = macos
        .iter()
        .find(|tier| tier.tier == RunnerExecutionMode::Sandbox.as_str())
        .expect("sandbox tier reported");
    assert!(!sandbox.usable);
    assert!(sandbox.reason.contains("Linux"), "{}", sandbox.reason);
}

#[test]
fn vm_tier_requires_kvm_and_a_firecracker_runtime() {
    let linux = TierFacts {
        os_linux: true,
        docker_ok: true,
        runtimes: vec!["runc".into()],
        kvm_readable: true,
    };
    let no_runtime = assess_tiers(&linux);
    let vm = no_runtime
        .iter()
        .find(|tier| tier.tier == RunnerExecutionMode::Vm.as_str())
        .expect("vm tier reported");
    assert!(!vm.usable);
    assert!(vm.reason.contains("Firecracker"), "{}", vm.reason);

    let no_kvm = assess_tiers(&TierFacts {
        kvm_readable: false,
        runtimes: vec!["runc".into(), "kata-fc".into()],
        ..linux.clone()
    });
    let vm = no_kvm
        .iter()
        .find(|tier| tier.tier == RunnerExecutionMode::Vm.as_str())
        .expect("vm tier reported");
    assert!(!vm.usable);
    assert!(vm.reason.contains("/dev/kvm"), "{}", vm.reason);

    let ready = assess_tiers(&TierFacts {
        runtimes: vec!["runc".into(), "kata-fc".into()],
        ..linux
    });
    let vm = ready
        .iter()
        .find(|tier| tier.tier == RunnerExecutionMode::Vm.as_str())
        .expect("vm tier reported");
    assert!(vm.usable, "{}", vm.reason);
    assert!(vm.reason.contains("kata-fc"), "{}", vm.reason);
}

#[test]
fn docker_unreachable_blocks_every_tier() {
    let tiers = assess_tiers(&TierFacts {
        os_linux: true,
        docker_ok: false,
        runtimes: vec!["runc".into(), "runsc".into(), "kata-fc".into()],
        kvm_readable: true,
    });
    assert!(tiers.iter().all(|tier| !tier.usable));
    assert!(tiers.iter().all(|tier| tier.reason.contains("docker")));
}

#[test]
fn exit_code_reports_any_usable_tier() {
    let report = |tiers: Vec<TierReport>, probe: Option<ProbeReport>| DoctorReport {
        tiers,
        probe,
        ..DoctorReport::default()
    };
    let tier = |usable: bool| TierReport {
        tier: "container".into(),
        usable,
        reason: String::new(),
    };
    assert_eq!(report(vec![tier(true)], None).exit_code(), 0);
    assert_eq!(report(vec![tier(false)], None).exit_code(), 2);
    assert_eq!(
        report(
            vec![tier(true)],
            Some(ProbeReport {
                image: "img@sha256:x".into(),
                runtime: None,
                ok: false,
                detail: "boom".into(),
            })
        )
        .exit_code(),
        1,
        "a failed requested probe is explicit, even when a tier looks usable"
    );
}

#[test]
fn token_mode_must_be_private() {
    assert!(mode_is_private(0o400));
    assert!(mode_is_private(0o600));
    assert!(!mode_is_private(0o440));
    assert!(!mode_is_private(0o604));
}

#[test]
fn probe_images_must_be_digest_pinned() {
    assert!(probe_image_is_pinned(
        "ghcr.io/example/image@sha256:0123456789abcdef"
    ));
    assert!(!probe_image_is_pinned("ghcr.io/example/image:latest"));
    assert!(!probe_image_is_pinned(""));
}
