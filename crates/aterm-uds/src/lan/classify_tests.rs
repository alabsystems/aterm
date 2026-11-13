// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::net::Ipv6Addr;

fn iface(address: &str, mask: &str) -> InterfaceAddress {
    InterfaceAddress {
        name: "en0".to_owned(),
        address: address.parse().unwrap(),
        netmask: Some(mask.parse().unwrap()),
        flags: 0,
    }
}

#[test]
fn the_fixed_ranges_are_local_and_loopback_and_unspecified_are_not() {
    for local in [
        "169.254.3.4",
        "224.0.0.251",
        "239.255.255.250",
        "255.255.255.255",
        "fe80::1",
        "febf::1",
        "ff02::fb",
        "::ffff:169.254.1.1",
    ] {
        let addr: IpAddr = local.parse().unwrap();
        assert!(is_local_network(&addr), "{local}");
    }
    for not in ["127.0.0.1", "::1", "0.0.0.0", "::", "::ffff:127.0.0.1"] {
        let addr: IpAddr = not.parse().unwrap();
        assert!(!is_local_network(&addr), "{not}");
    }
    assert!(!fixed_local(&"8.8.8.8".parse().unwrap()));
    assert!(!fixed_local(&"fec0::1".parse().unwrap()));
    assert!(!fixed_local(&Ipv6Addr::LOCALHOST.into()));
}

#[test]
fn an_attached_subnet_holds_its_hosts_gateway_broadcast_and_this_machine() {
    let attached = [
        iface("192.168.64.116", "255.255.255.0"),
        iface("2001:db8:1::5", "ffff:ffff:ffff:ffff::"),
    ];
    for local in [
        "192.168.64.1",
        "192.168.64.116",
        "192.168.64.255",
        "192.168.64.7",
        "2001:db8:1::99",
    ] {
        assert!(
            on_attached_subnet(&local.parse().unwrap(), &attached),
            "{local}"
        );
    }
    for not in ["192.168.65.1", "10.0.0.1", "2001:db8:2::1", "8.8.8.8"] {
        assert!(
            !on_attached_subnet(&not.parse().unwrap(), &attached),
            "{not}"
        );
    }
    let unmasked = [iface("10.0.0.2", "0.0.0.0")];
    assert!(!on_attached_subnet(&"8.8.8.8".parse().unwrap(), &unmasked));
}

#[test]
fn the_seam_makes_loopback_local_on_its_thread_alone() {
    let loopback: IpAddr = Ipv4Addr::LOCALHOST.into();
    let seam = crate::lan::Seam {
        loopback_is_local: true,
        ..crate::lan::Seam::default()
    };
    assert!(crate::lan::seam_here(seam, || is_local_network(&loopback)));
    assert!(!is_local_network(&loopback));
}

#[test]
fn local_names_end_in_local_or_have_one_label() {
    for local in [
        "m11.local",
        "M11.LOCAL.",
        "a.b.local",
        "m11",
        "m11.",
        "localhostx",
        "notlocalhost.local",
    ] {
        assert!(is_local_name(local), "{local}");
    }
    for not in [
        "",
        ".",
        "github.com",
        "local.example.com",
        "192.168.1.1",
        "::1",
        "fe80::1",
        ".local",
        "xlocal.com",
        "m11.xlocal",
        "localhost",
        "LocalHost.",
        "app.localhost",
        "a.b.LOCALHOST.",
    ] {
        assert!(!is_local_name(not), "{not}");
    }
}
