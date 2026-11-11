// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::network::{DefaultRoute, DefaultRoutes, InterfaceAddress};

fn address(name: &str, ip: [u8; 4], mask: [u8; 4]) -> InterfaceAddress {
    InterfaceAddress {
        name: name.to_owned(),
        address: IpAddr::V4(Ipv4Addr::from(ip)),
        netmask: Some(IpAddr::V4(Ipv4Addr::from(mask))),
        flags: 0,
    }
}

/// THE SCRIPTED-FACTS SEAM: a script file's lines read back as facts and routes, and a
/// thread that scripts them reads them from every reader (the routes, a reader's facts, the
/// route an answer took, the live sense), while another thread reads the Mac's own.
/// NEGATIVE CONTROL: `route_via` that skips the script (its `if let Some(script)` arm removed)
/// asks the kernel for the scripted address, and "the scripted route" fails.
#[test]
fn the_debug_seam_scripts_the_facts_on_this_thread_alone() {
    let script = parse_script(
        "# a phone hotspot\n\
         route en0 172.20.10.2/28 via 172.20.10.1 hw 02:00:00:00:00:01\n\
         path usable expensive wifi\n\
         via 192.168.4.9 172.20.10.2\n\
         via 10.8.0.1 -\n\
         nonsense line\n",
    );
    let route = script.facts.route.clone().expect("the route");
    assert_eq!(route.address, Ipv4Addr::new(172, 20, 10, 2));
    assert_eq!(route.prefix, 28);
    assert_eq!(route.gateway, Some(Ipv4Addr::new(172, 20, 10, 1)));
    assert_eq!(route.gateway_hw, Some(vec![2, 0, 0, 0, 0, 1]));
    let path = script.facts.path.expect("the path");
    assert!(path.expensive && path.usable == Some(true) && !path.constrained);
    assert_eq!(path.link, crate::place::Link::Wifi);
    let home: IpAddr = "192.168.4.9".parse().expect("an address");
    script_here(script.clone(), || {
        assert_eq!(routes_now().0.as_ref(), Some(&route));
        assert_eq!(reader_facts(), script.facts);
        assert_eq!(
            route_via(home),
            Some(Ipv4Addr::new(172, 20, 10, 2)),
            "the scripted route"
        );
        assert_eq!(route_via("10.8.0.1".parse().expect("an address")), None);
        assert_eq!(Live::start().unwrap().facts(), script.facts);
    });
    let elsewhere = std::thread::spawn(scripted).join().expect("another thread");
    assert_eq!(elsewhere, None, "another thread reads the Mac's own");
    assert_eq!(parse_script(""), Script::default());
}

/// The seam file scripts every reader but a proof: a report takes the file's script where the
/// thread has none, a proof (what the vault's admission reads, [`here_now`]) never does and
/// reads the Mac's own, and a test's thread script scripts both.
/// NEGATIVE CONTROL: `script_of` handing a proof the file's script too (its `Reader::Proof`
/// arm → `file()`) gives the proof the file's facts, and "a proof never takes the file's"
/// fails.
#[test]
fn the_seam_file_never_scripts_a_proof() {
    let file = parse_script("route en0 192.168.1.20/24 via 192.168.1.1 hw 02:00:00:00:00:01\n");
    let thread = parse_script("route en0 10.0.0.2/24 via 10.0.0.1\n");
    let from_file = || Some(file.clone());
    assert_eq!(
        script_of(None, from_file, Reader::Report),
        Some(file.clone()),
        "a report takes the file's"
    );
    assert_eq!(
        script_of(None, from_file, Reader::Proof),
        None,
        "a proof never takes the file's"
    );
    for reader in [Reader::Report, Reader::Proof] {
        assert_eq!(
            script_of(Some(thread.clone()), from_file, reader),
            Some(thread.clone()),
            "{reader:?}: a test's thread script"
        );
    }
    let routed = script_here(thread.clone(), here_now);
    assert_eq!(
        routed.facts, thread.facts,
        "here_now takes the thread's script"
    );
}

/// A path reading is kept only on the route it was read on, and only while it stands: back on
/// the home LAN from a phone hotspot (another address, another gateway), or once the gateway's
/// hardware address is read, the path is read again at once, not after the hotspot's
/// "expensive" reading has run out; an unsatisfied reading (none) is never kept at all, since
/// [`path_once`] stores only a reading.
/// NEGATIVE CONTROL: `kept_path` keeping by time alone (its `on == key &&` dropped) hands the
/// hotspot's reading to the home route, and "a move reads the path again" fails.
#[test]
fn a_path_reading_is_kept_only_on_the_route_it_was_read_on() {
    let route = |address: [u8; 4], hw: Option<u8>| RouteFacts {
        interface: "en0".to_owned(),
        address: Ipv4Addr::from(address),
        prefix: 24,
        gateway: None,
        gateway_hw: hw.map(|hw| vec![0, 1, 2, 3, 4, hw]),
    };
    let hotspot = route([172, 20, 10, 2], Some(1));
    let expensive = PathFacts {
        usable: Some(true),
        expensive: true,
        constrained: false,
        link: crate::place::Link::Wifi,
    };
    let at = Instant::now();
    let held = (at, path_key(Some(&hotspot)), expensive);
    let kept = Duration::from_secs(30);
    let soon = at + Duration::from_secs(5);
    assert_eq!(
        kept_path(Some(&held), &path_key(Some(&hotspot)), (soon, kept)),
        Some(expensive),
        "the same route keeps its reading"
    );
    assert_eq!(
        kept_path(
            Some(&held),
            &path_key(Some(&route([192, 168, 1, 20], Some(9)))),
            (soon, kept)
        ),
        None,
        "a move reads the path again"
    );
    let unread = route([192, 168, 1, 20], None);
    let unread_held = (at, path_key(Some(&unread)), expensive);
    assert_eq!(
        kept_path(
            Some(&unread_held),
            &path_key(Some(&route([192, 168, 1, 20], Some(9)))),
            (soon, kept)
        ),
        None,
        "a gateway read since reads the path again"
    );
    assert_eq!(
        kept_path(Some(&held), &path_key(Some(&hotspot)), (at + kept, kept)),
        None,
        "a reading past its time is read again"
    );
    assert_eq!(
        kept_path(None, &path_key(None), (soon, kept)),
        None,
        "nothing held"
    );
}

/// With a tunnel holding the primary default, the network is the scoped physical default
/// under it, with that interface's address and prefix, and the tunnel is named; with none,
/// the primary's own.
/// NEGATIVE CONTROL: composing from the primary whatever holds it keys the place to the
/// tunnel's address (`100.64.0.7`), and this test fails.
#[test]
fn a_tunnel_holding_the_default_is_looked_through_to_the_physical_network() {
    let gw = Ipv4Addr::new(192, 168, 4, 1);
    let interfaces = [
        address("en0", [192, 168, 4, 31], [255, 255, 252, 0]),
        address("utun4", [100, 64, 0, 7], [255, 255, 255, 255]),
    ];
    let routes = DefaultRoutes {
        primary: Some(DefaultRoute {
            interface: "utun4".to_owned(),
            gateway: None,
            gateway_hw: None,
        }),
        scoped: vec![DefaultRoute {
            interface: "en0".to_owned(),
            gateway: Some(gw),
            gateway_hw: Some(vec![1, 2, 3, 4, 5, 6].into_boxed_slice()),
        }],
    };
    let (route, tunnel) = compose(&routes, &interfaces);
    let route = route.expect("the physical network");
    assert_eq!(route.address, Ipv4Addr::new(192, 168, 4, 31));
    assert_eq!(route.prefix, 22);
    assert_eq!(route.gateway, Some(gw));
    assert_eq!(tunnel.as_deref(), Some("utun4"));
    let plain = DefaultRoutes {
        primary: routes.scoped.first().cloned(),
        scoped: Vec::new(),
    };
    let (route, tunnel) = compose(&plain, &interfaces);
    assert_eq!(route.map(|r| r.interface), Some("en0".to_owned()));
    assert_eq!(tunnel, None);
    assert_eq!(
        compose(&DefaultRoutes::default(), &interfaces),
        (None, None)
    );
}
