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
#[path = "classify_tests.rs"]
mod tests;
