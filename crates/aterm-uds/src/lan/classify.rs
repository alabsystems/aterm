// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which destinations are local network: the ones macOS's Local Network privacy gates, so the
//! ones the broker serves and the only ones it serves.

use std::net::{IpAddr, Ipv4Addr};

use crate::network::InterfaceAddress;

/// Whether `addr` is on the local network: an address on a subnet of an interface that is up
/// (this Mac's own addresses and its gateway included), IPv4 link-local (`169.254/16`), IPv6
/// link-local (`fe80::/10`), IPv4 or IPv6 multicast, or the IPv4 limited (`255.255.255.255`) or
/// a subnet's broadcast. Loopback and the unspecified address are not, nor is anything else. An
/// IPv4-mapped IPv6 address is classified as its IPv4 address. An interface table that cannot
/// be read classifies only the fixed ranges.
#[must_use]
pub fn is_local_network(addr: &IpAddr) -> bool {
    let addr = addr.to_canonical();
    if addr.is_loopback() {
        return super::loopback_is_local();
    }
    if addr.is_unspecified() {
        return false;
    }
    fixed_local(&addr)
        || crate::network::interfaces().is_ok_and(|attached| on_attached_subnet(&addr, &attached))
}

/// The ranges that are local network wherever this machine is attached.
fn fixed_local(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => v4.is_link_local() || v4.is_multicast() || *v4 == Ipv4Addr::BROADCAST,
        IpAddr::V6(v6) => v6.is_unicast_link_local() || v6.is_multicast(),
    }
}

/// Whether `addr` shares a subnet with one of `attached` (its network bits under that
/// interface's mask equal the interface address's): that covers the subnet's own hosts, its
/// gateway, its broadcast and this machine's address on it. An interface with no mask, or a
/// zero-length one, makes nothing local.
#[must_use]
pub fn on_attached_subnet(addr: &IpAddr, attached: &[InterfaceAddress]) -> bool {
    let addr = addr.to_canonical();
    attached.iter().any(
        |interface| match (addr, interface.address, interface.netmask) {
            (IpAddr::V4(addr), IpAddr::V4(here), Some(IpAddr::V4(mask))) => {
                let mask = u32::from(mask);
                mask != 0 && u32::from(addr) & mask == u32::from(here) & mask
            }
            (IpAddr::V6(addr), IpAddr::V6(here), Some(IpAddr::V6(mask))) => {
                let mask = u128::from(mask);
                mask != 0 && u128::from(addr) & mask == u128::from(here) & mask
            }
            _ => false,
        },
    )
}

/// Whether `name` is a local name, one only the local network answers: a name ending in
/// `.local` (any case, a trailing dot allowed) or a single label (`m11`). `localhost` and every
/// name under `.localhost` (RFC 6761 §6.3, any case, a trailing dot allowed) are not: they name
/// this machine's loopback, which resolves in-process without the local network. An address
/// literal is no name. The explicit loopback test seam classifies localhost as LAN too.
#[must_use]
pub fn is_local_name(name: &str) -> bool {
    let name = name.strip_suffix('.').unwrap_or(name);
    if ends_in_label(name, "localhost") {
        return super::loopback_is_local();
    }
    if name.is_empty() || name.parse::<IpAddr>().is_ok() {
        return false;
    }
    !name.contains('.') || (ends_in_label(name, "local") && name.len() > ".local".len())
}

/// Whether `name`'s last label is `label` (any case): `name` is `label` or ends in `.label`.
fn ends_in_label(name: &str, label: &str) -> bool {
    let Some(tail) = name.len().checked_sub(label.len()) else {
        return false;
    };
    name.get(tail..)
        .is_some_and(|end| end.eq_ignore_ascii_case(label))
        && (tail == 0 || name.as_bytes()[tail - 1] == b'.')
}

#[cfg(test)]
mod tests {
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
}
