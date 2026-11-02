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
    let name = interfaces
        .iter()
        .find(|address| address.address == IpAddr::V4(source))
        .map_or("-", |address| address.name.as_str());
    let hardware = hardware.map_or_else(
        || "-".to_owned(),
        |hardware| {
            hardware
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(":")
        },
    );
    format!("{name} {source} {hardware}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(name: &str, address: IpAddr) -> InterfaceAddress {
        InterfaceAddress {
            name: name.into(),
            address,
            netmask: None,
            flags: 0,
        }
    }

    /// The default route's key moves with its interface, its address or its gateway (another
    /// Wi-Fi network or LAN), and with nothing else: an AirDrop, `llw` or VPN address coming or
    /// going, or a new temporary IPv6 address, leaves it as it was.
    /// NEGATIVE CONTROL: a key of every interface address moves with each of those.
    #[test]
    fn the_default_route_moves_with_a_real_network_change_only() {
        let source = Ipv4Addr::new(192, 168, 1, 20);
        let wifi = address("en0", IpAddr::V4(source));
        let gateway: &[u8] = &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
        let home = default_route_of(source, std::slice::from_ref(&wifi), Some(gateway));
        assert_eq!(home, "en0 192.168.1.20 00:11:22:33:44:55");
        let churned = [
            wifi.clone(),
            address("awdl0", "fe80::1".parse().unwrap()),
            address("llw0", "fe80::2".parse().unwrap()),
            address("utun4", IpAddr::V4(Ipv4Addr::new(100, 64, 0, 7))),
            address("en0", "2001:db8::abcd:1".parse().unwrap()),
        ];
        assert_eq!(default_route_of(source, &churned, Some(gateway)), home);
        let other_router: &[u8] = &[0x00, 0x11, 0x22, 0x33, 0x44, 0x66];
        assert_ne!(
            default_route_of(source, std::slice::from_ref(&wifi), Some(other_router)),
            home,
            "another network numbered alike"
        );
        let wired = address("en7", IpAddr::V4(source));
        assert_ne!(
            default_route_of(source, &[wired], Some(gateway)),
            home,
            "the default route moved to another interface"
        );
        let renumbered = Ipv4Addr::new(192, 168, 1, 21);
        assert_ne!(
            default_route_of(
                renumbered,
                &[address("en0", IpAddr::V4(renumbered))],
                Some(gateway)
            ),
            home
        );
        assert_eq!(
            default_route_of(source, &[], None),
            "- 192.168.1.20 -",
            "a part not read yet"
        );
        assert!(route_known(&home));
        assert!(
            !route_known(&default_route_of(source, std::slice::from_ref(&wifi), None)),
            "a gateway ARP has not resolved yet is an unknown reading, not a move"
        );
        assert_eq!(
            route_place(&default_route_of(source, std::slice::from_ref(&wifi), None)).as_deref(),
            Some("en0 192.168.1.20"),
            "its interface and address are read all the same"
        );
        assert_eq!(route_place(&home).as_deref(), Some("en0 192.168.1.20"));
        assert_eq!(route_place("- 192.168.1.20 -"), None, "no interface read");
    }
}
