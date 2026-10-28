use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use shared::protocol::{NetworkInterfaceInfo, RunnerExecutionCapabilities, SystemInfo};
use sysinfo::{Disks, Networks, System};

/// Options controlling which IPs are advertised first to the Manager UI.
#[derive(Debug, Clone, Default)]
pub struct SysInfoOpts {
    /// Manager base URL — used to prefer local IPs on the same subnet as the Manager host.
    pub manager_url: String,
    /// Interface names whose addresses should be listed first (e.g. `["enp26s0"]`).
    pub prefer_interfaces: Vec<String>,
    /// Last measured Runner↔Manager clock skew (`remote - local`); positive
    /// means this Runner is behind. `None` when not measured yet.
    pub clock_skew_seconds: Option<i64>,
    pub execution_capabilities: Option<RunnerExecutionCapabilities>,
}

/// Human-readable OS description for the startup banner, e.g. "Ubuntu 24.04.4 LTS".
/// Prefers the long distribution string; falls back to `<name> <version>`.
pub fn os_description() -> String {
    if let Some(long) = System::long_os_version() {
        return long;
    }
    let name = System::name().unwrap_or_default();
    let version = System::os_version().unwrap_or_default();
    let combined = format!("{name} {version}").trim().to_string();
    if combined.is_empty() {
        "unknown".to_string()
    } else {
        combined
    }
}

/// Collect host hardware, OS, and network information.
/// `started_at` is the runner process start instant, used to compute process uptime.
pub fn collect_system_info(started_at: std::time::Instant, opts: &SysInfoOpts) -> SystemInfo {
    let mut sys = System::new_all();
    // First refresh: establish baseline CPU / memory / disk counters
    sys.refresh_all();

    // Wait for a meaningful delta — too short gives unstable CPU readings
    std::thread::sleep(std::time::Duration::from_millis(500));
    // Second refresh: compute usage deltas for all subsystems
    sys.refresh_all();

    let hostname = System::host_name().unwrap_or_else(|| "unknown".into());
    let os = os_description();
    let kernel_version = System::kernel_version().unwrap_or_else(|| "unknown".into());

    let cpu_arch = std::env::consts::ARCH.to_string();
    let cpu_cores = sys.physical_core_count().unwrap_or(1);
    let cpu_model = sys
        .cpus()
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_else(|| "unknown".into());

    let total_memory = sys.total_memory(); // bytes
    let used_memory = sys.used_memory();
    let total_swap = sys.total_swap();
    let used_swap = sys.used_swap();

    // ── disk ──
    let disks = Disks::new_with_refreshed_list();
    let (disk_total_bytes, disk_used_bytes) = disks.iter().fold((0u64, 0u64), |(t, u), d| {
        (
            t + d.total_space(),
            u + d.total_space() - d.available_space(),
        )
    });
    let disk_total_gb = disk_total_bytes as f64 / 1_073_741_824.0;
    let disk_used_gb = disk_used_bytes as f64 / 1_073_741_824.0;

    // ── network interfaces ──
    let manager_host = host_ip_from_url(&opts.manager_url);
    let prefer_set: Vec<String> = opts
        .prefer_interfaces
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let mut candidates: Vec<IpCandidate> = Vec::new();
    let interfaces = Networks::new_with_refreshed_list();
    let network_interfaces: Vec<NetworkInterfaceInfo> = interfaces
        .iter()
        .map(|(name, data)| {
            let demoted = is_demoted_interface(name);
            let preferred_iface = prefer_set.iter().any(|p| p == name);
            let ips: Vec<String> = data
                .ip_networks()
                .iter()
                .filter_map(|net| {
                    let addr = net.addr;
                    if is_ignored_addr(addr) {
                        return None;
                    }
                    let same_subnet = manager_host
                        .map(|m| ip_in_network(m, addr, net.prefix))
                        .unwrap_or(false);
                    candidates.push(IpCandidate {
                        ip: addr.to_string(),
                        preferred_iface,
                        same_subnet,
                        demoted,
                    });
                    Some(addr.to_string())
                })
                .collect();
            NetworkInterfaceInfo {
                name: name.clone(),
                mac: data.mac_address().to_string(),
                ips,
            }
        })
        .collect();

    let ip_addresses = order_ip_addresses(candidates);

    let uptime = started_at.elapsed().as_secs();

    // ── CPU usage ──
    let cpu_usage_percent: f32 = {
        let total: f32 = sys.cpus().iter().map(|c| c.cpu_usage()).sum();
        let count = sys.cpus().len().max(1) as f32;
        let avg = total / count;
        avg.clamp(0.0, 100.0)
    };

    SystemInfo {
        runner_version: env!("CARGO_PKG_VERSION").to_string(),
        hostname,
        os,
        kernel_version,
        cpu_arch,
        cpu_cores,
        cpu_model,
        cpu_usage_percent,
        memory_total_mb: total_memory / 1_048_576,
        memory_used_mb: used_memory / 1_048_576,
        swap_total_mb: total_swap / 1_048_576,
        swap_used_mb: used_swap / 1_048_576,
        disk_total_gb: (disk_total_gb * 100.0).round() / 100.0,
        disk_used_gb: (disk_used_gb * 100.0).round() / 100.0,
        network_interfaces,
        ip_addresses,
        uptime_seconds: uptime,
        clock_skew_seconds: opts.clock_skew_seconds,
        execution_capabilities: opts.execution_capabilities.clone(),
    }
}

#[derive(Debug, Clone)]
pub struct IpCandidate {
    pub ip: String,
    pub preferred_iface: bool,
    pub same_subnet: bool,
    pub demoted: bool,
}

/// Order advertised IPs: preferred interface → same subnet as Manager → non-virtual → lexical.
pub fn order_ip_addresses(mut candidates: Vec<IpCandidate>) -> Vec<String> {
    candidates.sort_by(|a, b| {
        b.preferred_iface
            .cmp(&a.preferred_iface)
            .then(b.same_subnet.cmp(&a.same_subnet))
            .then(a.demoted.cmp(&b.demoted))
            .then(a.ip.cmp(&b.ip))
    });
    let mut out = Vec::new();
    for c in candidates {
        if !out.iter().any(|x| x == &c.ip) {
            out.push(c.ip);
        }
    }
    out
}

/// Prefixes of hypervisor/bridge/tunnel interfaces that rank last when
/// advertising IPs. Deliberately excludes bond*/team*/wwan*/usb* — those
/// carry real traffic and must stay candidates for the primary address.
/// Keep in sync with the dashboard's `isVirtualIface` (Runners.vue).
const DEMOTED_IFACE_PREFIXES: &[&str] = &[
    // linux/macOS bridges & OVS: br0, br-lan, br-int, bridge0
    "br",
    // container runtimes & CNI plugins
    "docker",
    "podman",
    "veth",
    "cni",
    "cali",
    "cilium",
    "flannel",
    "weave",
    "kube-",
    "vxlan",
    "geneve",
    // hypervisors & VM plumbing
    "vmnet",
    "vmenet",
    "vmbr",
    "vboxnet",
    "vnic",
    "macvlan",
    "macvtap",
    "vif",
    "virbr",
    "anpi",
    // tunnels & VPNs
    "tun",
    "tap",
    "utun",
    "wg",
    "tailscale",
    "zt",
    "ppp",
    "ipsec",
    "xfrm",
    "sit",
    "gre",
    // misc plumbing (proxmox firewall links, dummies, apple awdl)
    "fwbr",
    "fwln",
    "fwpr",
    "dummy",
    "awdl",
];

pub fn is_demoted_interface(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    // `lo` exact only — a `lo` prefix would catch Windows' "Local Area Connection".
    n == "lo" || n == "lo0" || DEMOTED_IFACE_PREFIXES.iter().any(|p| n.starts_with(p))
}

fn is_ignored_addr(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_link_local() || v4.is_unspecified(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unicast_link_local() || v6.is_unspecified(),
    }
}

pub fn host_ip_from_url(url: &str) -> Option<IpAddr> {
    let trimmed = url.trim();
    let without_scheme = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .unwrap_or(trimmed);
    let host = without_scheme.split('/').next()?.split(':').next()?.trim();
    if host.is_empty() || host == "localhost" {
        return None;
    }
    // Bracketed IPv6: [::1]
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host.parse().ok()
}

fn ip_in_network(target: IpAddr, local: IpAddr, prefix: u8) -> bool {
    match (target, local) {
        (IpAddr::V4(t), IpAddr::V4(l)) => ipv4_same_network(t, l, prefix),
        (IpAddr::V6(t), IpAddr::V6(l)) => ipv6_same_network(t, l, prefix),
        _ => false,
    }
}

pub fn ipv4_same_network(a: Ipv4Addr, b: Ipv4Addr, prefix: u8) -> bool {
    if prefix > 32 {
        return false;
    }
    let mask = if prefix == 0 {
        0u32
    } else {
        u32::MAX << (32 - prefix)
    };
    (u32::from(a) & mask) == (u32::from(b) & mask)
}

fn ipv6_same_network(a: Ipv6Addr, b: Ipv6Addr, prefix: u8) -> bool {
    if prefix > 128 {
        return false;
    }
    let a = u128::from(a);
    let b = u128::from(b);
    let mask = if prefix == 0 {
        0u128
    } else {
        u128::MAX << (128 - prefix)
    };
    (a & mask) == (b & mask)
}
