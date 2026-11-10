// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_netplace::facts::{parse_script, script_here};

/// A lab vault's configuration served at `url`, pinned to a key no build names for ALab.
fn lab_vault(source: Source) -> Config {
    Config {
        source,
        mirrors: Vec::new(),
        name: "e1b-lab".to_owned(),
        signer: Some([7; 32]),
        followed: Vec::new(),
        host: "m2".to_owned(),
    }
}

fn served() -> Config {
    lab_vault(Source::Url(
        "http://192.168.1.42:7194/v1/e1b-lab".to_owned(),
    ))
}

/// This thread's default route on `interface` at `address/prefix`, behind a known gateway.
fn route(interface: &str, cidr: &str) -> aterm_netplace::facts::Script {
    parse_script(&format!(
        "route {interface} {cidr} via 192.168.1.1 hw aa:bb:cc:dd:ee:01\npath usable wifi\n"
    ))
}

/// A lab profile under `root`: a marked identity (this test binary makes lab identities) and its
/// vault folder naming `cidr` as its home LAN, where one is given.
fn profile(root: &std::path::Path, name: &str, cidr: Option<&str>) -> Paths {
    let paths = Paths::for_home(&root.join(name));
    client::machine_identity(&paths).expect("identity");
    if let Some(cidr) = cidr {
        write(&paths, &Cidr::parse(cidr).expect("a lab home")).expect("written");
    }
    paths
}

/// A lab home is one private IPv4 network from /16 to /30, named by its network address, clear
/// of a phone hotspot's range; everything else is refused, whatever else it looks like.
///
/// NEGATIVE CONTROL: with `Cidr::parse`'s hotspot check removed (`if false && cidr.overlaps(…)`),
/// the hotspot's own range is taken: `172.20.10.0/28 is refused: Ok(Cidr { net: 172.20.10.0,
/// prefix: 28 })`.
#[test]
fn a_lab_home_is_one_private_ipv4_network_clear_of_a_hotspot() {
    for (text, net, prefix) in [
        ("192.168.1.0/24", Ipv4Addr::new(192, 168, 1, 0), 24),
        ("192.168.1.0/24\n", Ipv4Addr::new(192, 168, 1, 0), 24),
        ("10.20.0.0/16", Ipv4Addr::new(10, 20, 0, 0), 16),
        ("172.16.4.0/22", Ipv4Addr::new(172, 16, 4, 0), 22),
        ("192.168.7.4/30", Ipv4Addr::new(192, 168, 7, 4), 30),
    ] {
        assert_eq!(
            Cidr::parse(text).expect(text),
            Cidr { net, prefix },
            "{text:?}"
        );
    }
    for text in [
        "8.8.8.0/24",
        "100.64.0.0/16",
        "127.0.0.0/16",
        "172.20.10.0/28",
        "172.20.10.8/29",
        "172.20.0.0/16",
        "fd00::/64",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.1.0/31",
        "192.168.1.1/32",
        "192.168.1.5/24",
        "192.168.1.0/024",
        "192.168.1.0",
        "192.168.1.0/24 192.168.2.0/24",
        "192.168.1.0/24\n192.168.2.0/24",
        "",
        "home",
        &format!("192.168.1.0/24{}", " ".repeat(64)),
    ] {
        let parsed = Cidr::parse(text);
        assert!(parsed.is_err(), "{text} is refused: {parsed:?}");
    }
    let home = Cidr::parse("192.168.1.0/24").unwrap();
    assert_eq!(home.mask(), Ipv4Addr::new(255, 255, 255, 0));
    assert!(home.contains(Ipv4Addr::new(192, 168, 1, 254)));
    assert!(!home.contains(Ipv4Addr::new(192, 168, 2, 1)));
    assert_eq!(home.to_string(), "192.168.1.0/24");
}

/// The file is the lab tool's alone to write and only this user's private file is read: what
/// [`write`] wrote reads back, a missing file names no home, and a file another user could read
/// or one too long is refused.
///
/// NEGATIVE CONTROL: with `read` taking the file by a plain `std::fs::read` in place of
/// `fsx::read_private`, the file others can read is read: `a file others can read is refused`.
#[test]
fn the_lab_home_file_reads_back_only_as_its_owners_private_file() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().expect("temp");
    let paths = profile(root.path(), "m2", None);
    assert_eq!(read(&paths).expect("no file reads"), None);
    let cidr = Cidr::parse("192.168.1.0/24").unwrap();
    write(&paths, &cidr).expect("written");
    let file = paths.dir.join(FILE);
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(read(&paths).expect("it reads"), Some(cidr));
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read(&paths).is_err(), "a file others can read is refused");
    std::fs::remove_file(&file).unwrap();
    crate::fsx::write_atomic(&file, &[b'1'; MAX_BYTES + 1], 0o600).unwrap();
    assert!(read(&paths).is_err(), "a long file is refused");
}

/// A lab member's desk has a home only where every gate holds: a lab vault (not ALab's) served
/// at a URL, off the live genesis, a marked identity, the file, and this Mac's default route on
/// Ethernet or Wi-Fi inside the network the file names. That home holds another host inside the
/// network and nothing else: not this Mac's own address, loopback or a host outside it.
///
/// NEGATIVE CONTROL (each run alone): with `home`'s `live_genesis` gate removed, the live genesis
/// gets a home: `the live genesis has no home: Some(HomeNet { own: 192.168.1.20, mask:
/// 255.255.255.0 })`. With its lab-mark gate removed, a release identity gets one: `a release
/// identity has no home: Some(HomeNet { own: 192.168.1.20, mask: 255.255.255.0 })`. With
/// `route_in`'s `cidr.contains` made always true, a route outside the network gets one: `a route
/// outside the network: Some(HomeNet { own: 10.0.0.5, mask: 255.255.255.0 })`.
#[test]
fn a_lab_desk_has_a_home_only_on_the_network_its_vault_names() {
    let root = aterm_tempfile::tempdir().expect("temp");
    let lab = profile(root.path(), "m2", Some("192.168.1.0/24"));
    let bare = profile(root.path(), "m3", None);
    let release = Paths::for_home(&root.path().join("release"));
    client::machine_identity_as(&release, crate::Making::default()).expect("identity");
    write(&release, &Cidr::parse("192.168.1.0/24").unwrap()).expect("written");
    assert!(client::lab_mark(&lab).unwrap().is_some(), "PRECONDITION");
    assert!(
        client::lab_mark(&release).unwrap().is_none(),
        "PRECONDITION"
    );
    let alab = Config {
        signer: crate::b64::decode_padded(crate::ALAB_SIGNER)
            .and_then(|key| <[u8; 32]>::try_from(key.as_slice()).ok()),
        ..served()
    };
    assert!(client::alab_vault(&alab), "PRECONDITION");
    let at_home = route("en0", "192.168.1.20/24");
    let expected = HomeNet::at(
        Ipv4Addr::new(192, 168, 1, 20),
        Ipv4Addr::new(255, 255, 255, 0),
    );

    let found = script_here(at_home.clone(), || {
        home(&lab, Some(&served()), false).unwrap()
    });
    assert_eq!(found, Some(expected), "every gate holds");
    let found = found.unwrap();
    assert!(found.holds("192.168.1.45".parse().unwrap()));
    assert!(found.holds("::ffff:192.168.1.45".parse().unwrap()));
    for outside in ["192.168.1.20", "192.168.2.45", "127.0.0.1", "10.0.0.5"] {
        assert!(!found.holds(outside.parse().unwrap()), "{outside}");
    }

    let gated = |paths: &Paths, config: Option<&Config>, live: bool, script| {
        script_here(script, || home(paths, config, live).unwrap())
    };
    assert_eq!(gated(&lab, None, false, at_home.clone()), None, "no vault");
    assert_eq!(
        gated(&lab, Some(&alab), false, at_home.clone()),
        None,
        "the live ALab vault"
    );
    let path = lab_vault(Source::Path(root.path().join("fixture.vault")));
    assert_eq!(
        gated(&lab, Some(&path), false, at_home.clone()),
        None,
        "a vault read from a path"
    );
    let live = gated(&lab, Some(&served()), true, at_home.clone());
    assert_eq!(live, None, "the live genesis has no home: {live:?}");
    let unmarked = gated(&release, Some(&served()), false, at_home.clone());
    assert_eq!(
        unmarked, None,
        "a release identity has no home: {unmarked:?}"
    );
    assert_eq!(
        gated(&bare, Some(&served()), false, at_home.clone()),
        None,
        "no file"
    );
    let outside = gated(&lab, Some(&served()), false, route("en0", "10.0.0.5/24"));
    assert_eq!(outside, None, "a route outside the network: {outside:?}");
    assert_eq!(
        gated(
            &lab,
            Some(&served()),
            false,
            route("utun4", "192.168.1.20/24")
        ),
        None,
        "a tunnel's interface"
    );
    let mut tunnelled = at_home.clone();
    tunnelled.facts.tunnel = Some("utun4".to_owned());
    assert_eq!(
        gated(&lab, Some(&served()), false, tunnelled),
        None,
        "a tunnel holding the default route"
    );
    assert_eq!(
        gated(
            &lab,
            Some(&served()),
            false,
            route("bridge100", "192.168.1.20/24")
        ),
        None,
        "a bridge"
    );
    assert_eq!(
        gated(&lab, Some(&served()), false, parse_script("")),
        None,
        "no default route"
    );
}

/// A default route on a point-to-point interface is no lab home, even an `en` one at an address
/// inside the network: the same route on a broadcast interface is home. A route at loopback or
/// at the unspecified address is inside no network a lab home takes, so it is never home either.
///
/// NEGATIVE CONTROL: with `route_in`'s `!here.point_to_point` clause removed, the point-to-point
/// link is home: `a point-to-point link: Some(HomeNet { own: 192.168.1.20, mask: 255.255.255.0
/// })`.
#[test]
fn a_point_to_point_route_is_no_lab_home() {
    let cidr = Cidr::parse("192.168.1.0/24").unwrap();
    let on = |facts: aterm_netplace::Facts, point_to_point| {
        home_in(
            &cidr,
            &Here {
                facts,
                point_to_point,
                own: Vec::new(),
            },
        )
    };
    let at_home = route("en0", "192.168.1.20/24").facts;
    assert_eq!(
        on(at_home.clone(), false),
        Some(HomeNet::at(
            Ipv4Addr::new(192, 168, 1, 20),
            Ipv4Addr::new(255, 255, 255, 0)
        )),
        "CONTROL: a broadcast LAN is home"
    );
    let linked = on(at_home.clone(), true);
    assert_eq!(linked, None, "a point-to-point link: {linked:?}");
    for address in [Ipv4Addr::LOCALHOST, Ipv4Addr::UNSPECIFIED] {
        let mut facts = at_home.clone();
        facts.route.as_mut().expect("a route").address = address;
        assert_eq!(on(facts, false), None, "{address}");
        for text in ["10.0.0.0/16", "172.16.0.0/16", "192.168.0.0/16"] {
            assert!(!Cidr::parse(text).unwrap().contains(address), "{text}");
        }
    }
}
