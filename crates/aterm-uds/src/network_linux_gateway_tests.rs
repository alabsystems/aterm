// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn proc_route_and_arp_name_the_default_gateways_hardware_address() {
    let route = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
         tun0\t00000000\t0108000A\t0003\t0\t0\t50\t00000080\n\
         en0\t00000000\t00000000\t0001\t0\t0\t90\t00000000\n\
         en0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\n";
    assert_eq!(
        linux_default_routes(route, "").primary.unwrap().gateway,
        None,
        "the gatewayless primary must not borrow a later route's gateway"
    );
    // Negative control: with no default-destination row there is no gateway.
    let no_default = "Iface\tDestination\tGateway\tFlags\nen0\t0001A8C0\t00000000\t0001\n";
    assert_eq!(linux_default_routes(no_default, "").primary, None);
    let signed_route = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
         en0\t00000000\t+101A8C0\t0003\t0\t0\t100\t00000000\n\
         en0\t00000000\t0101A8C0\t+003\t0\t0\t100\t00000000\n";
    assert_eq!(linux_default_routes(signed_route, "").primary, None);

    let arp = "IP address       HW type     Flags       HW address            Mask     Device\n\
         192.168.1.1      0x1         0x0         de:ad:be:ef:00:01     *        wlan0\n\
         192.168.1.1      0x1         0x+2        de:ad:be:ef:00:01     *        wlan0\n\
         192.168.1.1      0x1         0x2         +a:bb:cc:11:22:33     *        wlan0\n\
         192.168.1.1      0x1         0x2         aa:bb:cc:11:22:33     *        en0\n";
    assert_eq!(
        linux_neighbour_hardware(arp, Ipv4Addr::new(192, 168, 1, 1), "en0").as_deref(),
        Some([0xaa, 0xbb, 0xcc, 0x11, 0x22, 0x33].as_slice())
    );
    // Negative control: a stranger, and an incomplete entry, give nothing.
    assert_eq!(
        linux_neighbour_hardware(arp, Ipv4Addr::new(192, 168, 1, 9), "en0"),
        None
    );
    let incomplete = "IP address       HW type     Flags       HW address            Mask     Device\n\
         192.168.1.1      0x1         0x0         00:00:00:00:00:00     *        en0\n\
         192.168.1.1      0x1         0x0         de:ad:be:ef:00:01     *        en0\n";
    assert_eq!(
        linux_neighbour_hardware(incomplete, Ipv4Addr::new(192, 168, 1, 1), "en0"),
        None
    );
}

/// The first up `/0` row of `/proc/net/route` is the primary default, with its own
/// interface, gateway and that gateway's ARP entry; a down row is passed over.
#[test]
fn proc_route_names_the_primary_defaults_interface_and_gateway() {
    let route = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
         eth9\t00000000\t0101A8C0\t0002\t0\t0\t10\t00000000\n\
         wlan0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\n";
    let arp = "IP address       HW type     Flags       HW address            Mask     Device\n\
         192.168.1.1      0x1         0x2         de:ad:be:ef:00:07     *        eth9\n\
         192.168.1.1      0x1         0x2         aa:bb:cc:11:22:33     *        wlan0\n";
    let routes = linux_default_routes(route, arp);
    let primary = routes.primary.expect("a default");
    assert_eq!(primary.interface, "wlan0");
    assert_eq!(primary.gateway, Some(Ipv4Addr::new(192, 168, 1, 1)));
    assert_eq!(
        primary.gateway_hw.as_deref(),
        Some([0xaa, 0xbb, 0xcc, 0x11, 0x22, 0x33].as_slice())
    );
    assert!(routes.scoped.is_empty());
    // Negative control: no default row, no primary.
    let none = "Iface\tDestination\tGateway\tFlags\nen0\t0001A8C0\t00000000\t0001\n";
    assert_eq!(linux_default_routes(none, arp), DefaultRoutes::default());
}
