// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// Without `--advertise` the member listens on loopback alone: an address the guest's
/// network reaches (any interface, the guest's own address on tart's network, IPv6's any) is
/// refused before anything binds.
#[test]
fn the_member_listens_on_loopback_alone() {
    let ask = Ask::parse(&args(&["/s", "127.0.0.1:7486", "/o", "--allow", "mb"])).unwrap();
    assert_eq!(
        ask,
        Ask {
            home: PathBuf::from("/s"),
            listen: "127.0.0.1:7486".parse().unwrap(),
            out: "/o".into(),
            allow: Some("mb".into()),
            advertise: None,
        }
    );
    assert!(Ask::parse(&args(&["/s", "[::1]:7486", "/o"])).is_ok());
    for beyond in ["0.0.0.0:7486", "192.168.64.5:7486", "[::]:7486"] {
        let refused = Ask::parse(&args(&["/s", beyond, "/o"])).unwrap_err();
        assert!(refused.contains("not loopback"), "{beyond}: {refused}");
    }
    assert!(Ask::parse(&args(&["/s", "127.0.0.1", "/o"])).is_err());
    assert!(Ask::parse(&args(&["/s", "127.0.0.1:7486"])).is_err());
    assert!(Ask::parse(&args(&["/s", "127.0.0.1:7486", "/o", "x"])).is_err());
    assert!(Ask::parse(&args(&["/s", "127.0.0.1:7486", "/o", "--allow"])).is_err());
    assert!(Ask::parse(&args(&["/s", "127.0.0.1:7486", "/o", "--loud"])).is_err());
}

/// A run whose bundle the new Mac never acknowledged is written as sent, not as a Mac let in,
/// so `lab mac nux` counts no join the new Mac never took; one it acknowledged, or one from a
/// joiner that asked for no acknowledgement, is a Mac let in.
///
/// NEGATIVE CONTROL: write `let-in` for every run (`ran` ignoring `delivered`):
/// `left: ("let-in", "name=mb")`, `right: ("sent-unacknowledged", "name=mb the other Mac
/// closed the join exchange")`.
#[test]
fn a_bundle_the_new_mac_never_acknowledged_is_no_mac_let_in() {
    assert_eq!(
        ran(
            "mb",
            &Delivered::Unacknowledged("the other Mac  closed\nthe join exchange".into())
        ),
        (
            "sent-unacknowledged",
            "name=mb the other Mac closed the join exchange".to_owned()
        )
    );
    for delivered in [Delivered::Acknowledged, Delivered::Written] {
        assert_eq!(
            ran("mb", &delivered),
            ("let-in", "name=mb".to_owned()),
            "{delivered:?}"
        );
    }
}

/// With `--advertise` the member listens at the guest's own address and registers there
/// under a lab members service alone: `_alab._tcp`, the vault's service or any name that is
/// no lab one is refused, and so is a wildcard or multicast address, before anything binds or
/// registers. Its registration is the node job's, with the lab service in `_alab._tcp`'s
/// place.
///
/// NEGATIVE CONTROL (2026-10-29): `Ask::parse` taking any `--advertise` word (its
/// `lab_service` check removed) fails here: `_alab._tcp was taken`.
#[test]
fn an_advertising_member_registers_a_lab_service_alone_at_one_address() {
    let lab = "_alab-lab-k3x9q2._tcp";
    let ask = Ask::parse(&args(&[
        "/s",
        "192.168.64.5:7486",
        "/o",
        "--advertise",
        lab,
    ]))
    .unwrap();
    assert_eq!(ask.listen, "192.168.64.5:7486".parse().unwrap());
    assert_eq!(ask.advertise.as_deref(), Some(lab));
    assert!(Ask::parse(&args(&["/s", "127.0.0.1:7486", "/o", "--advertise", lab])).is_ok());
    for service in [
        "_alab._tcp",
        "_alab-vault._tcp",
        "_alab-lab-._tcp",
        "_http._tcp",
    ] {
        let said = Ask::parse(&args(&[
            "/s",
            "192.168.64.5:7486",
            "/o",
            "--advertise",
            service,
        ]));
        assert!(
            said.as_ref()
                .is_err_and(|e| e.contains("no lab members service")),
            "{service} was taken: {said:?}"
        );
    }
    for wide in ["0.0.0.0:7486", "[::]:7486", "224.0.0.251:7486"] {
        let said = Ask::parse(&args(&["/s", wide, "/o", "--advertise", lab]));
        assert!(said.is_err(), "{wide} was taken: {said:?}");
    }
    assert!(Ask::parse(&args(&["/s", "192.168.64.5:7486", "/o", "--advertise"])).is_err());

    let ours = advert_args(lab, "alab-1a2b3c4d", 7486);
    assert_eq!(
        ours,
        [
            "-R",
            "alab-1a2b3c4d",
            lab,
            "local",
            "7486",
            "v=1",
            "role=member",
            "fleet=7486",
            "window=1"
        ]
    );
    let shipped = aterm_agent::node_service::alab_tcp_advert(
        "alab-1a2b3c4d",
        aterm_agent::node_service::AlabTcpTxt {
            role: aterm_agent::node_service::NodeRole::Member,
            fleet_port: 7486,
            window: true,
        },
    );
    let txt: Vec<String> = shipped
        .txt
        .iter()
        .map(|t| String::from_utf8_lossy(t).into_owned())
        .collect();
    assert_eq!(
        (shipped.name.as_str(), shipped.port, txt.as_slice()),
        ("alab-1a2b3c4d", 7486, &ours[5..]),
        "only the service differs from the node job's advert"
    );
}
