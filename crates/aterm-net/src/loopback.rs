// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The isolated drive's guard: in a debug build, `ATERM_DEBUG_LOOPBACK_ONLY=1` refuses every
//! fleet listen, dial, name lookup and Bonjour browse that would leave this machine's loopback, before its
//! syscall. A release build never reads it.

use std::io;
use std::net::{IpAddr, SocketAddr};

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
pub fn check(reach: Reach<'_>) -> io::Result<()> {
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
            Reach::Listen("0.0.0.0:7100".parse().unwrap()),
            Reach::Listen("[::]:7100".parse().unwrap()),
            Reach::Listen("192.0.2.1:7100".parse().unwrap()),
            Reach::Dial("192.0.2.1:7100".parse().unwrap()),
            Reach::Dial("[2001:db8::1]:7100".parse().unwrap()),
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
            Reach::Listen("127.0.0.1:7100".parse().unwrap()),
            Reach::Listen("[::1]:7100".parse().unwrap()),
            Reach::Dial("127.0.0.1:7100".parse().unwrap()),
            Reach::Dial("[::ffff:127.0.0.1]:7100".parse().unwrap()),
            Reach::Lookup("localhost"),
            Reach::Lookup("127.0.0.1"),
            Reach::Lookup("192.0.2.1"),
        ] {
            assert!(forced(true, || check(reach)).is_ok(), "{reach:?}");
        }
    }
}
