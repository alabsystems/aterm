// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local interface facts for vault retry decisions, without a network connection.

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use aterm_uds::network::RouteChanges;
pub use aterm_uds::network::{InterfaceAddress, default_gateway_hw, interfaces};

use std::net::{IpAddr, Ipv4Addr};

/// This Mac's network as its traffic leaves it, read with no packet and no question asked: the
/// interface the IPv4 default route takes, this Mac's address there, and the default gateway's
/// hardware address, which tells one LAN or Wi-Fi network from another even where both number
/// their hosts alike. An address that comes and goes beside it (AirDrop's `awdl` and `llw`, a
/// VPN's `utun` that does not carry the default route, a temporary IPv6 address) moves none of
/// it, so a wait keyed to it wakes only once this Mac moved to another network. `None` with no
/// IPv4 default route, and while a part cannot be read yet ([`route_known`]: the gateway's
/// hardware address before ARP resolves it, after a wake or a join), which says nothing of a move.
#[must_use]
pub fn default_route() -> Option<String> {
    let source = crate::server::primary_ipv4()?;
    Some(default_route_of(
        source,
        &interfaces().unwrap_or_default(),
        default_gateway_hw().as_deref(),
    ))
    .filter(|route| route_known(route))
}

/// Whether every part of `route` (a [`default_route_of`] key) was read: one with a part not read
/// yet (`-`, the gateway's hardware address before ARP resolves it after a wake or a join) is an
/// unknown reading of the whole key, so a wait keyed to the whole route keeps the last one it
/// knew. Its parts that were read still tell a move ([`route_place`]).
#[must_use]
pub fn route_known(route: &str) -> bool {
    !route.split(' ').any(|part| part == "-")
}

/// Where `route` (a [`default_route_of`] key) puts this Mac on its network: the interface the
/// default route takes and this Mac's address there, when both were read. A route read before ARP
/// resolves the new gateway's hardware address (`en0 10.0.4.9 -`) still tells a move to another
/// network where this part changed, and the route source tells only address and link changes,
/// not ARP's, so that read may be the only one a wait gets.
#[must_use]
pub fn route_place(route: &str) -> Option<String> {
    let mut parts = route.split(' ');
    let (name, address) = (parts.next()?, parts.next()?);
    (name != "-" && address != "-").then(|| format!("{name} {address}"))
}

/// [`default_route`] of the default route's source address `source`, this Mac's `interfaces`
/// (which name the interface that holds it) and the gateway's `hardware` address; a part that
/// cannot be read yet is `-`.
#[must_use]
pub fn default_route_of(
    source: Ipv4Addr,
    interfaces: &[InterfaceAddress],
    hardware: Option<&[u8]>,
) -> String {
    use std::fmt::Write as _;

    let name = interfaces
        .iter()
        .find(|address| address.address == IpAddr::V4(source))
        .map_or("-", |address| address.name.as_str());
    // IPv4's longest spelling takes 15 bytes; both separators and the exact hardware field
    // fit beside it. Build the key in this buffer rather than materializing its components.
    let hardware_len = hardware.map_or(1, |bytes| bytes.len().saturating_mul(3).saturating_sub(1));
    let mut out = String::with_capacity(name.len().saturating_add(17).saturating_add(hardware_len));
    let _ = write!(out, "{name} {source} ");
    match hardware {
        Some(bytes) => {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            for (index, &byte) in bytes.iter().enumerate() {
                if index != 0 {
                    out.push(':');
                }
                out.push(char::from(HEX[usize::from(byte >> 4)]));
                out.push(char::from(HEX[usize::from(byte & 0xf)]));
            }
        }
        None => out.push('-'),
    }
    out
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
