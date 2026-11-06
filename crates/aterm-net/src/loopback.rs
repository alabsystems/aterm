// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The isolated drive's guard: in a debug build, `ATERM_DEBUG_LOOPBACK_ONLY=1` refuses every
//! fleet listen, dial, name lookup and Bonjour browse that would leave this machine's loopback, before its
//! syscall. A release build never reads it.
//!
//! The person's ports need no seam: a private aterm (a Cargo test binary, one under a private
//! state root, a debug build that is no shell of its user's own account) neither listens on nor
//! dials the fleet's default port or LAN toolset sharing's at any address, however it came by the
//! place ([`aterm_types::reach`], the one rule atpkg's toolset peers ask too). The loopback is no
//! shelter: an aterm listening on `*:7100` answers there, and each proof it refuses is a line of
//! its log.

use std::io;
use std::net::IpAddr;

pub use aterm_types::reach::Reach;

/// The variable that turns the guard on (`1`), read by a debug build alone.
pub const SEAM: &str = "ATERM_DEBUG_LOOPBACK_ONLY";

/// `Err` (`PermissionDenied`, naming the guard and the address or word) where the guard is on and
/// `reach` leaves the loopback: a listen on or a dial of any address but 127/8 and `::1`, or a
/// lookup of a word that is no IP literal or `localhost`, or any Bonjour browse
/// (whose native API cannot confine itself to loopback). `Ok` wherever the guard is off.
/// Whatever the guard says, `Err` (`PermissionDenied`) where a private aterm would listen on or
/// dial one of the person's ports ([`aterm_types::reach::check`], the module docs).
pub fn check(reach: Reach<'_>) -> io::Result<()> {
    aterm_types::reach::check(reach)?;
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

/// Whether this process runs under the guard: every fleet listen, dial, lookup and browse it
/// makes stays on the loopback ([`check`]), so none of its fleet traffic is local network.
#[must_use]
pub fn confined() -> bool {
    on()
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
}
