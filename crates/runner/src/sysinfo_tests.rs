use super::*;

#[test]
fn host_ip_from_http_url() {
    assert_eq!(
        host_ip_from_url("http://192.0.2.1:8080"),
        Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)))
    );
    assert_eq!(
        host_ip_from_url("https://10.0.0.5/api"),
        Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)))
    );
    assert!(host_ip_from_url("http://buz.example.com:8080").is_none());
}

#[test]
fn ipv4_same_network_slash24() {
    let a = Ipv4Addr::new(192, 0, 2, 1);
    let b = Ipv4Addr::new(192, 0, 2, 130);
    let c = Ipv4Addr::new(198, 51, 100, 140);
    assert!(ipv4_same_network(a, b, 24));
    assert!(!ipv4_same_network(a, c, 24));
}

#[test]
fn order_prefers_same_subnet_over_lexicographic() {
    let ordered = order_ip_addresses(vec![
        IpCandidate {
            ip: "198.51.100.140".into(),
            preferred_iface: false,
            same_subnet: false,
            demoted: false,
        },
        IpCandidate {
            ip: "192.0.2.130".into(),
            preferred_iface: false,
            same_subnet: true,
            demoted: false,
        },
        IpCandidate {
            ip: "172.17.0.1".into(),
            preferred_iface: false,
            same_subnet: false,
            demoted: true,
        },
    ]);
    assert_eq!(
        ordered,
        vec![
            "192.0.2.130".to_string(),
            "198.51.100.140".to_string(),
            "172.17.0.1".to_string(),
        ]
    );
}

#[test]
fn order_prefer_interfaces_wins() {
    let ordered = order_ip_addresses(vec![
        IpCandidate {
            ip: "192.0.2.130".into(),
            preferred_iface: false,
            same_subnet: true,
            demoted: false,
        },
        IpCandidate {
            ip: "10.0.0.5".into(),
            preferred_iface: true,
            same_subnet: false,
            demoted: false,
        },
    ]);
    assert_eq!(ordered[0], "10.0.0.5");
}

#[test]
fn demoted_interfaces_cover_common_virtual_adapters() {
    for name in [
        "docker0",
        "docker_gwbridge",
        "lo",
        "lo0",
        "br-3f2a",
        "br0",
        "bridge0",
        "veth8f2c1a",
        "virbr0",
        "cni0",
        "cali3f2a1b0c",
        "cilium_host",
        "flannel.1",
        "weave",
        "kube-ipvs0",
        "vxlan.calico",
        "geneve0",
        "podman0",
        "tun0",
        "tap9",
        "utun3",
        "wg0",
        "tailscale0",
        "ztabc123def456",
        "ppp0",
        "ipsec0",
        "xfrm0",
        "sit0",
        "gre0",
        "vmnet8",
        "vmenet0",
        "vEthernet (WSL)",
        "vboxnet0",
        "vnic0",
        "vmbr0",
        "macvlan0",
        "macvtap0",
        "vif2.0",
        "anpi0",
        "fwbr0",
        "fwln0",
        "fwpr0",
        "dummy0",
        "awdl0",
    ] {
        assert!(is_demoted_interface(name), "{name} should be demoted");
    }
    // Real traffic carriers must stay candidates for the primary address.
    for name in [
        "eth0", "eno1", "enp5s0", "wlan0", "ens18", "en0", "bond0", "team0", "wwan0", "usb0",
    ] {
        assert!(!is_demoted_interface(name), "{name} must not be demoted");
    }
}
