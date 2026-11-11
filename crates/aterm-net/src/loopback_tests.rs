// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_guard_refuses_what_leaves_the_loopback_and_nothing_while_off() {
    let off_loopback = [
        Reach::Listen("0.0.0.0:7101".parse().unwrap()),
        Reach::Listen("[::]:7101".parse().unwrap()),
        Reach::Listen("192.0.2.1:7101".parse().unwrap()),
        Reach::Dial("192.0.2.1:7101".parse().unwrap()),
        Reach::Dial("[2001:db8::1]:7101".parse().unwrap()),
        Reach::Lookup("mac.invalid"),
        Reach::Lookup("mac"),
        Reach::Lookup("localhost.local"),
        Reach::Lookup("localhost."),
        Reach::Browse("_alab._tcp"),
    ];
    for reach in off_loopback {
        let refused = forced(true, || check(reach)).expect_err("refused");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{reach:?}");
        let said = refused.to_string();
        assert!(
            said.starts_with("ATERM_DEBUG_LOOPBACK_ONLY=1 refuses "),
            "{said}"
        );
        let place = match reach {
            Reach::Listen(at) | Reach::Dial(at) => at.to_string(),
            Reach::Lookup(word) | Reach::Browse(word) => format!("{word:?}"),
        };
        assert!(said.contains(&place), "{said}");
        assert!(
            forced(false, || check(reach)).is_ok(),
            "the control: {reach:?}"
        );
    }
    for reach in [
        Reach::Listen("127.0.0.1:7101".parse().unwrap()),
        Reach::Listen("[::1]:7101".parse().unwrap()),
        Reach::Dial("127.0.0.1:7101".parse().unwrap()),
        Reach::Dial("[::ffff:127.0.0.1]:7101".parse().unwrap()),
        Reach::Lookup("localhost"),
        Reach::Lookup("127.0.0.1"),
        Reach::Lookup("192.0.2.1"),
    ] {
        assert!(forced(true, || check(reach)).is_ok(), "{reach:?}");
    }
}

/// Whatever the isolated drive's guard says, as a test binary is a private aterm: the rule is
/// [`aterm_types::reach::check`]'s, it comes first, and it holds each of the person's ports,
/// the fleet's and LAN toolset sharing's, so the seam's words never stand in for it.
///
/// NEGATIVE CONTROL (2026-11-03): with [`check`] asking the rule of the fleet's port alone, the
/// shape before, the toolset port's listen and dial pass with the guard off and this fails.
#[test]
fn a_test_binary_is_private_with_the_guard_off_and_on() {
    use aterm_types::reach::{FLEET_PORT, TOOLSET_PORT};
    for port in [FLEET_PORT, TOOLSET_PORT] {
        let at = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        for reach in [Reach::Listen(at), Reach::Dial(at)] {
            for on in [false, true] {
                let refused = forced(on, || check(reach)).expect_err("refused");
                assert_eq!(
                    refused.kind(),
                    io::ErrorKind::PermissionDenied,
                    "guard {on}: {refused}"
                );
                assert!(
                    refused.to_string().contains(&at.to_string()),
                    "guard {on}: {refused}"
                );
            }
        }
    }
}
