// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The partition lane's cut (`targo --unverified lab swarm partition`): in a debug build,
//! `ATERM_DEBUG_PARTITION=<file>` names a file of `ip:port` lines this process cannot reach, as a
//! Mac that left the LAN cannot reach the Macs still on it. A TCP connect to a listed address
//! fails at once with `HostUnreachable`, the kind macOS gives a connect with no route, before any
//! socket is made. The file is read again on every connect, so the lab cuts a running process off
//! and heals it by rewriting the file; a missing or empty file cuts nothing. A release build
//! never reads it.
//!
//! The two connect funnels read it: aterm-net's (`tcp_connect::connect_timeout_kept`: every fleet
//! dial, and a loopback-scope `lan::Connection::connect`) and aterm-http's
//! (`connect::connect_timeout`, every vault request). A LAN-scope `lan::Connection` dials through
//! `lan::native`, which it does not cut, and an accepted connection is never cut: only the side
//! that dials is.

use std::io;
use std::net::SocketAddr;

/// The variable naming the cut's file, read by a debug build alone.
pub const SEAM: &str = "ATERM_DEBUG_PARTITION";

/// `Err` (`HostUnreachable`, naming the seam and the address) where the cut lists `to`; `Ok`
/// wherever the seam is off, its file is missing, or the file does not list `to`.
///
/// # Errors
/// The cut lists `to`.
pub fn check(to: SocketAddr) -> io::Result<()> {
    match cut_text() {
        Some(text) if listed(&text, to) => Err(io::Error::new(
            io::ErrorKind::HostUnreachable,
            format!("{SEAM}: {to} is on the other side of the lab's partition (no route to host)"),
        )),
        _ => Ok(()),
    }
}

/// Whether `text`, the cut's file (one `ip:port` per line; blank lines, `#` comments and words
/// that are no socket address skipped), lists `to`. An IPv4-mapped IPv6 address is its IPv4 one.
#[must_use]
pub fn listed(text: &str, to: SocketAddr) -> bool {
    let canonical = |at: SocketAddr| (at.ip().to_canonical(), at.port());
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.parse::<SocketAddr>().ok())
        .any(|at| canonical(at) == canonical(to))
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static HERE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// `f` with this thread cut off from what `text` lists (the file's form), whatever the
/// environment says: the seam's arm for tests, this crate's and the funnels'.
#[cfg(any(test, debug_assertions))]
pub fn cut_here<R>(text: &str, f: impl FnOnce() -> R) -> R {
    let was = HERE.with(|here| here.replace(Some(text.to_owned())));
    let ran = f();
    HERE.with(|here| here.replace(was));
    ran
}

/// What the cut lists now: this thread's [`cut_here`], else (a debug build outside this crate's
/// tests) the file [`SEAM`] names, read now.
fn cut_text() -> Option<String> {
    #[cfg(any(test, debug_assertions))]
    if let Some(text) = HERE.with(|here| here.borrow().clone()) {
        return Some(text);
    }
    #[cfg(all(debug_assertions, not(test)))]
    {
        let file = std::env::var_os(SEAM)?;
        Some(std::fs::read_to_string(file).unwrap_or_default())
    }
    #[cfg(not(all(debug_assertions, not(test))))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listed_address_is_unreachable_and_every_other_one_passes() {
        let cut = "# island B\n127.0.0.1:7484\n\n  127.0.0.1:7100  \nnot an address\n[::ffff:127.0.0.1]:41384\n";
        let at = |s: &str| s.parse::<SocketAddr>().unwrap();
        for listed_one in ["127.0.0.1:7484", "127.0.0.1:7100", "127.0.0.1:41384"] {
            let refused = cut_here(cut, || check(at(listed_one))).unwrap_err();
            assert_eq!(
                refused.kind(),
                io::ErrorKind::HostUnreachable,
                "{listed_one}"
            );
            assert!(refused.to_string().contains(SEAM), "{refused}");
        }
        for other in ["127.0.0.1:7485", "[::1]:7484", "10.0.0.1:7100"] {
            assert!(cut_here(cut, || check(at(other))).is_ok(), "{other}");
        }
        assert!(
            cut_here("", || check(at("127.0.0.1:7484"))).is_ok(),
            "healed"
        );
        assert!(check(at("127.0.0.1:7484")).is_ok(), "no cut on this thread");
    }
}
