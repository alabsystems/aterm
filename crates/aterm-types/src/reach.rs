// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PERSON'S PORTS: the two fixed ports the process that runs the person's fleet listener
//! answers on, the fleet's ([`FLEET_PORT`]) and, beside it, LAN toolset sharing's
//! ([`TOOLSET_PORT`]). A private aterm (a Cargo test binary, one under a private state root, a
//! debug build that is no shell of its user's own account) neither listens on nor dials either at
//! any address, however it came by the place (a word it was given, a book, a `[fleet] listen` or
//! `[packages] peers` of its own, the system resolver): [`check`] refuses it before the syscall.
//! It is one rule for both ports' sockets: the fleet's guard (`aterm_net::loopback::check`) asks
//! it first of every fleet listen and dial, and atpkg's toolset peers ask it of each of theirs.
//! The loopback is no shelter: an aterm listening on `*:7100` answers there, and on macOS a
//! listen on `127.0.0.1:7487` takes that address's connections from one on `*:7487`.

use std::io;
use std::net::SocketAddr;

/// The fleet port when none is named: `[fleet] listen`'s documented example.
pub const FLEET_PORT: u16 = 7100;

/// The port a toolset peer named without one is reached on, and every toolset server listens on
/// by default.
pub const TOOLSET_PORT: u16 = 7487;

/// Each of the person's ports, with whose it is and what a private aterm does instead, as a
/// refusal says it.
const PERSONS: [(u16, &str); 2] = [
    (
        FLEET_PORT,
        "the person's fleet's, and a private aterm names its own (`--port`)",
    ),
    (
        TOOLSET_PORT,
        "the person's LAN toolset sharing's, and a private aterm names its own \
         (`serve-units --listen`, `[packages] peers`)",
    ),
];

/// What a socket is about to do with a place, as [`check`] and the fleet's guard judge it.
#[derive(Clone, Copy, Debug)]
pub enum Reach<'a> {
    /// Bind and listen on this address.
    Listen(SocketAddr),
    /// Connect to this address.
    Dial(SocketAddr),
    /// Ask the system resolver for this word's addresses.
    Lookup(&'a str),
    /// Ask Bonjour for this service on the network.
    Browse(&'a str),
}

/// `Err` (`PermissionDenied`, naming the address and whose port it is) where `reach` listens on or
/// dials one of the person's ports and this process is a private aterm (the module docs). A lookup
/// or a browse names no port, and what it finds comes here as a dial.
///
/// # Errors
/// A private aterm would listen on or dial [`FLEET_PORT`] or [`TOOLSET_PORT`].
pub fn check(reach: Reach<'_>) -> io::Result<()> {
    the_persons_port(reach, private)
}

/// [`check`] with `private` asked whether this process is a private aterm, only once `reach`
/// listens on or dials one of the person's ports.
fn the_persons_port(reach: Reach<'_>, private: impl FnOnce() -> bool) -> io::Result<()> {
    let (act, at) = match reach {
        Reach::Listen(at) => ("a listen on", at),
        Reach::Dial(at) => ("a dial of", at),
        Reach::Lookup(_) | Reach::Browse(_) => return Ok(()),
    };
    let Some((port, whose)) = PERSONS.into_iter().find(|(port, _)| *port == at.port()) else {
        return Ok(());
    };
    if !private() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("this private aterm refuses {act} {at}: port {port} is {whose}"),
    ))
}

/// Whether this process is a private aterm, which holds none of the person's ports: a Cargo test
/// binary, whether or not its root could be made; one under a private state root; a debug build
/// that is no shell of its user's own account ([`crate::dirs::not_own_account`]). A release build
/// asks only the private state root, which it never has unless it compiles the dev seams: a person
/// may run the shipped aterm under any `HOME`.
fn private() -> bool {
    #[cfg(debug_assertions)]
    if std::env::current_exe().is_ok_and(|exe| aterm_uds::test_binary::is_test_binary(&exe))
        || crate::dirs::not_own_account().is_some()
    {
        return true;
    }
    crate::dirs::runs_under_private_state_root()
}

#[cfg(test)]
mod tests {
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
}
