// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::network::InterfaceAddress;
use std::net::Ipv4Addr;

fn on(name: &str, ip: [u8; 4], mask: [u8; 4]) -> InterfaceAddress {
    InterfaceAddress {
        name: name.into(),
        address: Ipv4Addr::from(ip).into(),
        netmask: Some(Ipv4Addr::from(mask).into()),
        flags: 0,
    }
}

#[test]
fn a_network_is_its_ipv4_subnets_without_loopback_or_link_local() {
    let lan = [
        on("en0", [192, 168, 4, 23], [255, 255, 255, 0]),
        on("lo0", [127, 0, 0, 1], [255, 0, 0, 0]),
        on("en5", [169, 254, 3, 9], [255, 255, 0, 0]),
        on("utun4", [100, 64, 0, 7], [255, 255, 255, 255]),
        on("en1", [192, 168, 4, 99], [255, 255, 255, 0]),
    ];
    assert_eq!(
        network_of(&lan).as_deref(),
        Some("100.64.0.7/32,192.168.4.0/24")
    );
    assert_eq!(
        network_of(&[on("lo0", [127, 0, 0, 1], [255, 0, 0, 0])]),
        None
    );
}

/// A route on `subnet`'s `.23` behind a gateway whose hardware address is `hw`.
fn route(subnet: [u8; 3], hw: Option<&[u8]>) -> aterm_netplace::RouteFacts {
    aterm_netplace::RouteFacts {
        interface: "en0".to_owned(),
        address: Ipv4Addr::new(subnet[0], subnet[1], subnet[2], 23),
        prefix: 24,
        gateway: Some(Ipv4Addr::new(subnet[0], subnet[1], subnet[2], 1)),
        gateway_hw: hw.map(<[u8]>::to_vec),
    }
}

/// The network key is the network place's strong id of the route traffic takes, read from the
/// one source of where this Mac is: a scripted route gives that route's id, a gateway whose
/// hardware address is unread gives none, and no route gives none.
/// NEGATIVE CONTROL: keying by the place's weak id where the strong one cannot be formed
/// (`.or_else(|| routes_now().0.as_ref().map(NetworkId::weak))` in `network_now`) gives a key
/// before the gateway is read, and "no key before ARP" fails.
#[test]
fn a_network_key_is_the_network_places_strong_id() {
    let hardware = [0xaa, 0xbb, 0xcc, 0x11, 0x22, 0x33];
    let script = |route: Option<aterm_netplace::RouteFacts>| aterm_netplace::facts::Script {
        facts: aterm_netplace::Facts {
            route,
            ..aterm_netplace::Facts::default()
        },
        via: Vec::new(),
    };
    let home = route([192, 168, 4], Some(&hardware));
    let key = aterm_netplace::facts::script_here(script(Some(home.clone())), network_key);
    assert_eq!(
        key,
        aterm_netplace::NetworkId::of(&home).map(|id| id.as_str().to_owned())
    );
    assert!(key.is_some());
    assert_eq!(
        aterm_netplace::facts::script_here(script(Some(route([192, 168, 4], None))), network_key),
        None,
        "no key before ARP"
    );
    assert_eq!(
        aterm_netplace::facts::script_here(script(None), network_key),
        None
    );
}

#[test]
fn a_kept_look_stands_for_its_kind_and_carries_what_it_found() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    assert_eq!(read(&paths), None);
    let here = "n00112233445566aa";
    let absent = Verdict {
        at: 1_000,
        heard: Heard::Absent,
        network: here.into(),
    };
    write(&paths, &absent).unwrap();
    let kept = read(&paths).expect("kept");
    assert_eq!(kept, absent);
    assert_eq!(
        kept.standing(here, 1_000 + ABSENT_FOR - 1),
        Some(&Heard::Absent)
    );
    assert_eq!(kept.standing(here, 1_000 + ABSENT_FOR), None);
    assert_eq!(kept.standing(here, 999), None, "a look from the future");
    // A Found look carries the vault's url and signer through the record file.
    let found = Verdict {
        at: 2_000,
        heard: Heard::Found {
            url: "https://vault.example/v1/alab".into(),
            signer: "abc123fingerprint".into(),
        },
        network: here.into(),
    };
    write(&paths, &found).unwrap();
    assert_eq!(
        read(&paths).as_ref(),
        Some(&found),
        "the url and signer survive the round trip"
    );
    assert_eq!(
        found.standing(here, 2_000 + FOUND_FOR - 1),
        Some(&found.heard)
    );
    assert_eq!(found.standing(here, 2_000 + FOUND_FOR), None);
}

#[test]
fn an_absent_stands_on_its_gateway_not_on_a_twin_subnet() {
    let key = |hw: &[u8]| {
        aterm_netplace::NetworkId::of(&route([192, 168, 1], Some(hw)))
            .unwrap()
            .as_str()
            .to_owned()
    };
    let here = key(&[0xaa, 0xbb, 0xcc, 0, 0, 1]);
    let twin = key(&[0xde, 0xad, 0xbe, 0xef, 0, 2]);
    assert_ne!(
        here, twin,
        "two gateways numbering one subnet are two networks"
    );
    let absent = Verdict {
        at: 1_000,
        heard: Heard::Absent,
        network: here.clone(),
    };
    assert_eq!(absent.standing(&here, 1_001), Some(&Heard::Absent));
    // The negative control is a key of the subnet alone: there the stale Absent would stand on
    // the new network and block the vault. With the gateway in the key it does not.
    assert_eq!(absent.standing(&twin, 1_001), None);
}

#[test]
fn forget_drops_the_kept_look() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let here = "n00112233445566aa";
    write(
        &paths,
        &Verdict {
            at: 1_000,
            heard: Heard::Absent,
            network: here.into(),
        },
    )
    .unwrap();
    assert!(read(&paths).is_some());
    forget(&paths);
    assert_eq!(read(&paths), None, "a grant invalidated the record");
    forget(&paths); // Nothing to drop is not an error.
}

#[test]
fn an_unreadable_or_unmigratable_record_is_dropped() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let file = paths.dir.join(FILE);
    crate::fsx::ensure_private_dir(&paths.dir).unwrap();
    for line in [
        "1000 maybe n00112233445566aa\n",               // an unknown word
        "1000 absent n00112233445566aa\n",              // an older build's failed look
        "1000 elsewhere 192.168.4.0/24\n",              // a subnet-only key: dropped
        "1000 elsewhere aabbcc112233@192.168.4.0/24\n", // an earlier build's key: dropped
        "1000 elsewhere w00112233445566aa\n",           // a weak id (no gateway read): dropped
        "1000 found n00112233445566aa\n",               // Found missing its url and signer
        "1000 found n00112233445566aa https://v\n",     // Found missing its signer
    ] {
        std::fs::write(&file, line).unwrap();
        assert_eq!(read(&paths), None, "{line:?}");
    }
}
