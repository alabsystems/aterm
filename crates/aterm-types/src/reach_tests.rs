// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The rule alone, for a private aterm and for the person's own: it refuses a private aterm
/// each of the person's ports at any address, loopback or not, asks nothing of an aterm that
/// goes nowhere near them, and refuses nothing of the person's.
///
/// NEGATIVE CONTROL (2026-11-03): with [`PERSONS`] holding the fleet's port alone, the shape
/// before, the toolset port's dials and listens are not refused and this fails.
#[test]
fn a_private_aterm_reaches_none_of_the_persons_ports_and_the_persons_aterm_every_one() {
    for port in [FLEET_PORT, TOOLSET_PORT] {
        let reaches = [
            Reach::Dial(SocketAddr::from(([127, 0, 0, 1], port))),
            Reach::Dial(format!("[::1]:{port}").parse().unwrap()),
            Reach::Dial(format!("[::ffff:127.0.0.1]:{port}").parse().unwrap()),
            Reach::Dial(SocketAddr::from(([192, 168, 1, 99], port))),
            Reach::Listen(SocketAddr::from(([0, 0, 0, 0], port))),
            Reach::Listen(SocketAddr::from(([127, 0, 0, 1], port))),
        ];
        for reach in reaches {
            let refused = the_persons_port(reach, || true).expect_err("refused");
            assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{reach:?}");
            let (Reach::Listen(at) | Reach::Dial(at)) = reach else {
                unreachable!("the array holds listens and dials")
            };
            assert!(refused.to_string().contains(&at.to_string()), "{refused}");
            assert!(
                the_persons_port(reach, || false).is_ok(),
                "the control: {reach:?}"
            );
        }
    }
    let elsewhere = [
        Reach::Dial(SocketAddr::from(([127, 0, 0, 1], FLEET_PORT + 1))),
        Reach::Listen(SocketAddr::from(([0, 0, 0, 0], FLEET_PORT - 1))),
        Reach::Dial(SocketAddr::from(([127, 0, 0, 1], TOOLSET_PORT + 1))),
        Reach::Listen(SocketAddr::from(([127, 0, 0, 1], 0))),
        Reach::Lookup("localhost"),
        Reach::Browse("_alab._tcp"),
    ];
    for reach in elsewhere {
        assert!(
            the_persons_port(reach, || panic!("no port of the person's: {reach:?}")).is_ok(),
            "{reach:?}"
        );
    }
}

/// A Cargo test binary is a private aterm, so [`check`] itself refuses it each of the person's
/// ports, before any socket is made.
#[test]
fn a_test_binary_reaches_none_of_the_persons_ports() {
    for port in [FLEET_PORT, TOOLSET_PORT] {
        let at = SocketAddr::from(([127, 0, 0, 1], port));
        for reach in [Reach::Listen(at), Reach::Dial(at)] {
            let refused = check(reach).expect_err("a test binary is private");
            assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
            assert!(refused.to_string().contains(&at.to_string()), "{refused}");
        }
    }
}
