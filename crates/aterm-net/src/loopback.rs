// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The isolated drive's guard: in a debug build, `ATERM_DEBUG_LOOPBACK_ONLY=1` refuses every
//! fleet listen, dial, name lookup and Bonjour browse that would leave this machine's loopback, before its
//! syscall. A release build never reads it.
//!
//! The person's fleet port needs no seam: a private aterm (a Cargo test binary, one under a
//! private state root, a debug build that is no shell of its user's own account) neither listens
//! on nor dials the default fleet port at any address, however it came by the place (a word it
//! was given, a book, a `[fleet] listen` of its own or the system resolver). The loopback is no
//! shelter: an aterm listening on `*:7100` answers there, and each proof it refuses is a line of
//! its log. Off Unix no fleet dials, and the rule compiles away.

use std::io;
use std::net::{IpAddr, SocketAddr};

#[cfg(unix)]
use crate::fleet_member::DEFAULT_PORT;

/// The variable that turns the guard on (`1`), read by a debug build alone.
pub const SEAM: &str = "ATERM_DEBUG_LOOPBACK_ONLY";

/// What a fleet socket is about to do, as [`check`] judges it.
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

/// `Err` (`PermissionDenied`, naming the guard and the address or word) where the guard is on and
/// `reach` leaves the loopback: a listen on or a dial of any address but 127/8 and `::1`, or a
/// lookup of a word that is no IP literal or `localhost`, or any Bonjour browse
/// (whose native API cannot confine itself to loopback). `Ok` wherever the guard is off.
/// Whatever the guard says, `Err` (`PermissionDenied`) where a private aterm would listen on or
/// dial the default fleet port (the module docs).
pub fn check(reach: Reach<'_>) -> io::Result<()> {
    #[cfg(unix)]
    the_persons_port(reach, private)?;
    if !on() {
        return Ok(());
    }
    let loopback = |ip: IpAddr| ip.to_canonical().is_loopback();
    let (act, place) = match reach {
        Reach::Listen(at) if loopback(at.ip()) => return Ok(()),
        Reach::Dial(at) if loopback(at.ip()) => return Ok(()),
        Reach::Lookup(word) if word.parse::<IpAddr>().is_ok() || word == "localhost" => {
            return Ok(());
        }
        Reach::Listen(at) => ("a listen on", at.to_string()),
        Reach::Dial(at) => ("a dial of", at.to_string()),
        Reach::Lookup(word) => ("a lookup of", format!("{word:?}")),
        Reach::Browse(service) => ("a Bonjour browse of", format!("{service:?}")),
    };
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "{SEAM}=1 refuses {act} {place}: this debug aterm reaches the loopback alone \
             (127.0.0.1, ::1, localhost)"
        ),
    ))
}

/// `Err` where `reach` listens on or dials [`DEFAULT_PORT`] and this process is `private`, which is
/// asked only then: a lookup or a browse names no port, and what it finds comes here as a dial.
#[cfg(unix)]
fn the_persons_port(reach: Reach<'_>, private: impl FnOnce() -> bool) -> io::Result<()> {
    let (act, at) = match reach {
        Reach::Listen(at) => ("a listen on", at),
        Reach::Dial(at) => ("a dial of", at),
        Reach::Lookup(_) | Reach::Browse(_) => return Ok(()),
    };
    if at.port() != DEFAULT_PORT || !private() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "this private aterm refuses {act} {at}: port {DEFAULT_PORT} is the person's fleet's, \
             and a private aterm names its own (`--port`)"
        ),
    ))
}

/// Whether this process is a private aterm, which holds none of the person's fleet: a Cargo test
/// binary, whether or not its root could be made; one under a private state root; a debug build
/// that is no shell of its user's own account ([`aterm_types::dirs::not_own_account`]). A release
/// build asks only the private state root, which it never has unless it compiles the dev seams:
/// a person may run the shipped aterm under any `HOME`.
#[cfg(unix)]
fn private() -> bool {
    #[cfg(debug_assertions)]
    if std::env::current_exe().is_ok_and(|exe| aterm_uds::test_binary::is_test_binary(&exe))
        || aterm_types::dirs::not_own_account().is_some()
    {
        return true;
    }
    aterm_types::dirs::runs_under_private_state_root()
}

#[cfg(test)]
thread_local! {
    static FORCED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `f` with the guard on (`true`) or off on this thread; this crate's own tests read no
/// environment, so the guard is off in them elsewhere.
#[cfg(test)]
pub(crate) fn forced<R>(on: bool, f: impl FnOnce() -> R) -> R {
    let was = FORCED.with(|forced| forced.replace(on));
    let ran = f();
    FORCED.with(|forced| forced.set(was));
    ran
}

fn on() -> bool {
    #[cfg(test)]
    {
        FORCED.with(std::cell::Cell::get)
    }
    #[cfg(all(debug_assertions, not(test)))]
    {
        std::env::var_os(SEAM).is_some_and(|value| value == "1")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
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

    /// The rule alone, for a private aterm and for the person's own: it asks nothing of an aterm
    /// that goes nowhere near the port, and refuses nothing of the person's.
    #[cfg(unix)]
    #[test]
    fn a_private_aterm_reaches_no_default_port_and_the_persons_aterm_every_one() {
        let port = DEFAULT_PORT;
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
        let elsewhere = [
            Reach::Dial(SocketAddr::from(([127, 0, 0, 1], port + 1))),
            Reach::Listen(SocketAddr::from(([0, 0, 0, 0], port - 1))),
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

    /// Whatever the isolated drive's guard says, as a test binary is a private aterm: the rule is
    /// [`check`]'s, and it comes first, so the seam's words never stand in for it.
    #[cfg(unix)]
    #[test]
    fn a_test_binary_is_private_with_the_guard_off_and_on() {
        let at = SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT));
        for on in [false, true] {
            let refused = forced(on, || check(Reach::Dial(at))).expect_err("refused");
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
