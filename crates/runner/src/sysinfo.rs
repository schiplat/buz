use shared::protocol::{NetworkInterfaceInfo, SystemInfo};
use sysinfo::{Disks, Networks, System};

/// Collect host hardware, OS, and network information.
/// `started_at` is the runner process start instant, used to compute process uptime.
pub fn collect_system_info(started_at: std::time::Instant) -> SystemInfo {
    let mut sys = System::new_all();
    // First refresh: establish baseline CPU / memory / disk counters
    sys.refresh_all();

    // Wait for a meaningful delta — too short gives unstable CPU readings
    std::thread::sleep(std::time::Duration::from_millis(500));
    // Second refresh: compute usage deltas for all subsystems
    sys.refresh_all();

    let hostname = System::host_name().unwrap_or_else(|| "unknown".into());
    let os = System::long_os_version()
        .or_else(|| Some(System::name()? + " " + &System::os_version()?))
        .unwrap_or_else(|| "unknown".into());
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
        (t + d.total_space(), u + d.total_space() - d.available_space())
    });
    let disk_total_gb = disk_total_bytes as f64 / 1_073_741_824.0;
    let disk_used_gb = disk_used_bytes as f64 / 1_073_741_824.0;

    // ── network interfaces ──
    let mut ip_addresses: Vec<String> = Vec::new();
    let interfaces = Networks::new_with_refreshed_list();
    let network_interfaces: Vec<NetworkInterfaceInfo> = interfaces
        .iter()
        .map(|(name, data)| {
            let ips: Vec<String> = data
                .ip_networks()
                .iter()
                .map(|net| net.addr.to_string())
                .collect();
            ip_addresses.extend(ips.clone());
            NetworkInterfaceInfo {
                name: name.clone(),
                mac: data.mac_address().to_string(),
                ips,
            }
        })
        .collect();
    // Deduplicate IPs: keep only non-loopback IPv4/IPv6 addresses for the global list
    ip_addresses.sort();
    ip_addresses.dedup();
    ip_addresses.retain(|ip| {
        !ip.starts_with("127.") && !ip.starts_with("::1") && !ip.starts_with("fe80:")
    });

    let uptime = started_at.elapsed().as_secs();

    // ── CPU usage ──
    // sysinfo computes usage as delta between two refresh calls,
    // so we need a second refresh after a brief wait.
    let cpu_usage_percent: f32 = {
        let total: f32 = sys.cpus().iter().map(|c| c.cpu_usage()).sum();
        let count = sys.cpus().len().max(1) as f32;
        let avg = total / count;
        avg.clamp(0.0, 100.0) // safety clamp; some platforms can briefly exceed 100%
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
    }
}
