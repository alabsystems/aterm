// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn address(name: &str, address: IpAddr) -> InterfaceAddress {
    InterfaceAddress {
        name: name.into(),
        address,
        netmask: None,
        flags: 0,
    }
}

/// The previous formatter, kept independent of the streamed encoding.
fn previous_route(
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

#[test]
fn streamed_route_keys_match_the_previous_encoding() {
    for source in [
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::new(1, 2, 3, 4),
        Ipv4Addr::new(192, 168, 1, 20),
        Ipv4Addr::BROADCAST,
    ] {
        for name in ["en0", "", "en 0", "界🚀"] {
            let matching = address(name, IpAddr::V4(source));
            for interfaces in [
                vec![address(name, IpAddr::V6(std::net::Ipv6Addr::LOCALHOST))],
                vec![matching.clone()],
                vec![matching.clone(), address("later", IpAddr::V4(source))],
            ] {
                assert_eq!(
                    default_route_of(source, &interfaces, None),
                    previous_route(source, &interfaces, None)
                );
                for seed in 0..=u8::MAX {
                    for len in [0, 1, 6, 20, 256] {
                        let bytes: Vec<_> = (0..len)
                            .map(|at| seed.wrapping_add(u8::try_from(at).unwrap()))
                            .collect();
                        assert_eq!(
                            default_route_of(source, &interfaces, Some(&bytes)),
                            previous_route(source, &interfaces, Some(&bytes)),
                            "source={source} name={name:?} seed={seed} len={len}"
                        );
                    }
                }
            }
        }
    }
}

/// Pure key formatting, not interface enumeration, route observation or network I/O.
/// Allocation observation and instruction sampling use separate identical passes.
#[test]
#[ignore = "manual paired route-key allocation and instruction measurement"]
fn measure_route_key_work() {
    use aterm_bench::measurement_clock::{counters, declare_interactive};
    use std::hint::black_box;
    type Probe = fn(Ipv4Addr, &[InterfaceAddress], Option<&[u8]>) -> String;
    let _ = declare_interactive();
    let probes: [(&str, Probe); 2] = [
        ("reference", previous_route),
        ("candidate", default_route_of),
    ];
    for scenario in ["wifi", "missing", "duplicate", "empty", "churn"] {
        let (source, interfaces) = match scenario {
            "wifi" => {
                let source = Ipv4Addr::new(192, 168, 1, 20);
                (source, vec![address("en0", IpAddr::V4(source))])
            }
            "missing" => (Ipv4Addr::new(10, 0, 0, 1), Vec::new()),
            "duplicate" => {
                let source = Ipv4Addr::BROADCAST;
                (
                    source,
                    vec![
                        address("界🚀", IpAddr::V4(source)),
                        address("later", IpAddr::V4(source)),
                    ],
                )
            }
            "empty" => (
                Ipv4Addr::UNSPECIFIED,
                vec![address("", IpAddr::V4(Ipv4Addr::UNSPECIFIED))],
            ),
            "churn" => {
                let source = Ipv4Addr::new(1, 2, 3, 4);
                (
                    source,
                    vec![
                        address("en0", IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
                        address("utun4", IpAddr::V4(Ipv4Addr::new(100, 64, 0, 7))),
                        address("en 0", IpAddr::V4(source)),
                    ],
                )
            }
            _ => unreachable!(),
        };
        for len in [None, Some(0), Some(1), Some(6), Some(20), Some(256)] {
            let bytes: Vec<_> = (0..len.unwrap_or(0))
                .map(|at| u8::try_from(at).unwrap().wrapping_mul(17))
                .collect();
            let hardware = len.map(|_| bytes.as_slice());
            assert_eq!(
                previous_route(source, &interfaces, hardware),
                default_route_of(source, &interfaces, hardware)
            );
            for (_, probe) in probes {
                black_box(probe(source, &interfaces, hardware));
            }
            for sample in 0..5 {
                for arm in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
                    let (variant, probe) = probes[arm];
                    let run = || {
                        for _ in 0..2000 {
                            black_box(probe(
                                black_box(source),
                                black_box(&interfaces),
                                black_box(hardware),
                            ));
                        }
                    };
                    let (_, allocations) = aterm_heap_count::allocations_in(run);
                    let before = counters();
                    run();
                    let after = counters();
                    let instructions = before
                        .zip(after)
                        .map(|(a, b)| b.instructions.saturating_sub(a.instructions));
                    eprintln!(
                        "route_key scenario={scenario} len={len:?} variant={variant} sample={sample} iterations=2000 allocations={allocations} instructions={instructions:?} instruction_allocations_counted=false"
                    );
                }
            }
        }
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
