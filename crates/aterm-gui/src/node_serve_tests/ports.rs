// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The loopback ports a node-serve test hands a node job: each one claimed from its pick until
//! the test is done with it, so the test can bind the port again to show the job let it go.

use std::net::{Ipv4Addr, TcpListener, UdpSocket};

/// The first of the ports [`listener_no_port_zero_bind_takes`] hands out.
const PORTS_FROM: u32 = 20_000;
/// How many ports above [`PORTS_FROM`] it hands out. They lie outside every range this host's
/// kernel picks ports from itself for a bind of port 0 or a dial ([`ephemeral_ranges`]: from
/// 49152 on macOS and from 32768 on Linux by default), so no such bind, here or in any other
/// process, lands on one.
const PORTS_SPAN: u32 = 12_000;

/// A loopback port held for one test, from its pick until this drops, against every other pick
/// in this process or any other: a UDP socket bound to the same port. A second bind of it fails,
/// as neither asks for `SO_REUSEPORT`, and the kernel lets it go when this process ends, so no
/// claim outlives its test. TCP keeps its own ports, so the claim never stands in the way of the
/// test's own binds of the port.
pub(super) struct PortClaim {
    _held: UdpSocket,
}

/// The span's ports in turn, from the one `start` places first.
fn ports_from(start: u32) -> impl Iterator<Item = u16> {
    (0..PORTS_SPAN).map(move |step| {
        let offset = (start % PORTS_SPAN + step) % PORTS_SPAN;
        u16::try_from(PORTS_FROM + offset).expect("the span is ports")
    })
}

/// The first of `ports` that `claim` and then `bind` both take, with its claim. A port whose
/// bind fails gives its claim back before the next is tried.
fn take_port<C, L>(
    mut ports: impl Iterator<Item = u16>,
    mut claim: impl FnMut(u16) -> Option<C>,
    mut bind: impl FnMut(u16) -> Option<L>,
) -> Option<(L, C)> {
    ports.find_map(|port| {
        let claimed = claim(port)?;
        Some((bind(port)?, claimed))
    })
}

/// A [`PortClaim`] on `port`, unless another pick holds one.
fn claim(port: u16) -> Option<PortClaim> {
    aterm_uds::kept_out(|| UdpSocket::bind((Ipv4Addr::LOCALHOST, port)))
        .ok()
        .map(|held| PortClaim { _held: held })
}

/// A loopback listener on `port`, made as the node job makes its own sockets.
fn bind(port: u16) -> Option<TcpListener> {
    aterm_uds::kept_out(|| TcpListener::bind((Ipv4Addr::LOCALHOST, port))).ok()
}

/// The ranges this host's kernel picks ports from itself for a bind of port 0 or a dial, each as
/// its lowest and highest port: `net.inet.ip.portrange.first` to `last`, and `hifirst` to
/// `hilast` for a socket that asks for the high range. A range whose first port is above its
/// last is counted down, so each is the span between its two ends.
#[cfg(target_os = "macos")]
fn ephemeral_ranges() -> Vec<(u16, u16)> {
    let read = |name: &std::ffi::CStr| {
        let mut value: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>();
        // SAFETY: `name` is NUL-terminated and `value` is `len` writable bytes, which the call
        // fills; it sets no value.
        let read = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&raw mut value).cast(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(read, 0, "{name:?}: {}", std::io::Error::last_os_error());
        u16::try_from(value).expect("a port")
    };
    let range = |first, last| {
        let (first, last) = (read(first), read(last));
        (first.min(last), first.max(last))
    };
    vec![
        range(
            c"net.inet.ip.portrange.first",
            c"net.inet.ip.portrange.last",
        ),
        range(
            c"net.inet.ip.portrange.hifirst",
            c"net.inet.ip.portrange.hilast",
        ),
    ]
}

/// The range this host's kernel picks ports from itself for a bind of port 0 or a dial, as its
/// lowest and highest port: `net.ipv4.ip_local_port_range`, which IPv6 shares and whose first
/// port is never above its last.
#[cfg(target_os = "linux")]
fn ephemeral_ranges() -> Vec<(u16, u16)> {
    let range = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .expect("the local port range");
    let ports: Vec<u16> = range
        .split_whitespace()
        .map(|port| port.parse().expect("a port"))
        .collect();
    let [first, last] = ports[..] else {
        panic!("two ports in {range:?}");
    };
    vec![(first, last)]
}

/// A loopback listener for a node job, and the claim on its port that the test holds until it
/// has bound the port again: while the claim lives, no other pick takes the port, in this binary
/// or in another, even in the moment after the job lets it go. The old helper took the first port
/// its bind took, from a start each process's id fixed, so another process walking the same span
/// (another aterm-gui test binary on this Mac) could take the port just freed; a cursor kept in
/// this process could not have stopped that. The span lies outside [`ephemeral_ranges`], so no
/// bind of port 0 or dial takes the port either; a host whose kernel picks inside it is refused
/// here.
///
/// One thing still keeps a freed port bound: a fork holding a copy of the listener until it
/// execs or exits. Every socket here is made under [`aterm_uds::kept_out`], so on a kernel that
/// keeps close-on-fork no spawn that takes the spawn lock copies it; a fork that takes no lock
/// (std's spawn, a bare `fork`) can copy it between its creation and its mark, and on a kernel
/// with no close-on-fork (Linux) every fork copies it.
pub(super) fn listener_no_port_zero_bind_takes() -> (TcpListener, PortClaim) {
    for (low, high) in ephemeral_ranges() {
        assert!(
            u32::from(high) < PORTS_FROM || PORTS_FROM + PORTS_SPAN <= u32::from(low),
            "this host picks ports {low}..={high} itself for a bind of port 0, which meets the \
             test ports {PORTS_FROM}..{}",
            PORTS_FROM + PORTS_SPAN
        );
    }
    take_port(ports_from(std::process::id()), claim, bind)
        .expect("a free loopback port below the ephemeral range")
}

/// A FREED PORT GOES TO NO OTHER PICK WHILE ITS CLAIM LIVES: once a node job lets its port go,
/// the old helper, which only bound, hands that port to the next pick that reaches it (the
/// CONTROL), so the test that owned it could not bind it again. A pick that claims first passes
/// it over, and the test that holds the claim binds its port again.
#[test]
fn a_freed_port_goes_to_no_other_pick_while_its_claim_lives() {
    let (listener, _claimed) = listener_no_port_zero_bind_takes();
    let port = listener.local_addr().expect("its address").port();
    drop(listener);
    let from_it = || ports_from(u32::from(port) - PORTS_FROM);

    // The old helper's first candidate from the shared start: it tries no other, so it can never
    // take a port another test here holds only by its claim.
    let (old, ()) = take_port(from_it().take(1), |_| Some(()), bind).expect("a port");
    assert_eq!(
        old.local_addr().expect("its address").port(),
        port,
        "CONTROL: the old helper hands the next pick the port a job just freed"
    );
    drop(old);

    let (other, _other_claim) = take_port(from_it(), claim, bind).expect("a port");
    assert_ne!(
        other.local_addr().expect("its address").port(),
        port,
        "a pick took a port another test's claim holds"
    );
    assert!(
        claim(port).is_none(),
        "a second claim of a port, from any process, is refused while the first lives"
    );
    assert!(
        bind(port).is_some(),
        "the claim's holder binds its port again"
    );
}
