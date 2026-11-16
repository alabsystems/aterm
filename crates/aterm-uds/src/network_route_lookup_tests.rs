// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who hears a route lookup: live asks of this Mac's kernel over routing sockets, which send no
//! packet. What a lookup must not be heard by is a second routing socket, as every other program
//! on the Mac that watches the routing table holds one. That a message was not heard is shown
//! without a clock: a plain `RTM_GET`, which every routing socket hears, is asked afterwards,
//! and the bystander is read up to it.

use super::*;
use std::os::fd::AsRawFd as _;

/// `RTM_MISS` (`net/route.h`): a lookup the kernel made for a socket found no route.
const RTM_MISS: u8 = 0x7;

/// A routing socket that asks nothing and hears every family's messages, as another program's
/// does.
struct Bystander(std::os::fd::OwnedFd);

impl Bystander {
    fn open() -> Self {
        let socket = routing_socket(0).unwrap();
        // Room for whatever else the Mac's programs make the kernel say meanwhile.
        let room: libc::c_int = 256 * 1024;
        // SAFETY: `room` is a live c_int, and its size is the length passed.
        let set = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                (&raw const room).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        assert_eq!(set, 0, "{}", std::io::Error::last_os_error());
        Self(socket)
    }

    /// Every message heard so far, in the kernel's order, up to and including this process's
    /// lookup `sequence`, which must be one every routing socket hears: a one-minute hang
    /// detector stands over it.
    fn heard_up_to(&self, sequence: i32) -> Vec<Vec<u8>> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut heard = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(
                !left.is_zero(),
                "the RTM_GET {sequence} was never heard by a second routing socket"
            );
            let mut poll = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `poll` is one live pollfd and the count passed is 1.
            let ready =
                unsafe { libc::poll(&raw mut poll, 1, crate::deadline::poll_timeout_ms(left)) };
            if ready <= 0 {
                continue;
            }
            let mut buf = [0u8; 2048];
            // SAFETY: `buf` is live and writable for its whole length.
            let got =
                unsafe { libc::recv(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            let got = usize::try_from(got).expect("the bystander's read");
            let message = buf[..got].to_vec();
            let last = route_get_reply(&message, pid(), sequence).is_some();
            heard.push(message);
            if last {
                return heard;
            }
        }
    }
}

fn pid() -> i32 {
    i32::try_from(std::process::id()).unwrap()
}

/// What an answer says of its route, apart from the lookup it answers: the route's interface,
/// flags, address mask and address block.
type Found = (u16, i32, i32, Vec<u8>);

fn found(message: &[u8], pid: i32, sequence: i32) -> Option<Result<Found, i32>> {
    Some(
        route_get_reply(message, pid, sequence)?.map(|(head, block)| {
            let at = std::mem::offset_of!(RtMsghdr, rtm_index);
            (
                u16::from_ne_bytes([head[at], head[at + 1]]),
                head_i32(head, std::mem::offset_of!(RtMsghdr, rtm_flags)),
                head_i32(head, std::mem::offset_of!(RtMsghdr, rtm_addrs)),
                block.to_vec(),
            )
        }),
    )
}

/// One lookup of `to` written as message type `kind` ([`lookup_write_as`]: on this kernel, which
/// takes the silent type, nothing else): the number it was written as (0 where its write failed),
/// and the routing table's word ([`table_word`]).
fn ask(to: IpAddr, kind: u8) -> (i32, Option<Option<Found>>) {
    let mut sequence = 0;
    let asked = route_get_as(
        to,
        None,
        |request, write| {
            sequence = lookup_write_as(kind == RTM_GET, request, write)?;
            Ok(sequence)
        },
        found,
    );
    (sequence, table_word(asked))
}

/// An address of the IPv6 documentation prefix (`2001:db8::/32`, RFC 3849) that no other lookup
/// on this Mac names, so a message that carries its sixteen bytes is about this test's ask.
fn documentation_address() -> Ipv6Addr {
    use std::hash::{BuildHasher as _, Hasher as _};
    let random = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let pid = std::process::id();
    Ipv6Addr::new(
        0x2001,
        0xdb8,
        (pid >> 16) as u16,
        pid as u16,
        (random >> 48) as u16,
        (random >> 32) as u16,
        (random >> 16) as u16,
        random as u16,
    )
}

fn names(message: &[u8], to: Ipv6Addr) -> bool {
    message.windows(16).any(|window| window == to.octets())
}

/// A lookup written as `RTM_GET_SILENT` answers its asker and is heard by no other routing
/// socket, for mDNS's group (the LAN's route), a destination the default route takes and
/// loopback: the bystander, read up to the plain `RTM_GET` of the same destination asked next,
/// holds no message that answers the silent lookup.
///
/// The plain lookup being read at all is the control built in: a bystander that heard no lookup
/// would fail the hang detector. NEGATIVE CONTROL (2026-11-12): with `RTM_GET_SILENT` set to
/// `RTM_GET`'s 0x4, this fails at "heard by a second routing socket" for `224.0.0.251`.
#[test]
fn a_silent_lookup_is_answered_to_its_asker_alone() {
    let bystander = Bystander::open();
    for to in [
        Ipv4Addr::new(224, 0, 0, 251),
        Ipv4Addr::new(192, 0, 2, 1),
        Ipv4Addr::LOCALHOST,
    ] {
        let (silent, answer) = ask(to.into(), RTM_GET_SILENT);
        assert!(answer.is_some(), "the asker is answered: {to}");
        let (plain, _) = ask(to.into(), RTM_GET);
        let heard = bystander.heard_up_to(plain);
        assert!(
            !heard
                .iter()
                .any(|message| route_get_reply(message, pid(), silent).is_some()),
            "the silent lookup of {to} was heard by a second routing socket"
        );
    }
}

/// Either type of lookup reads the same route: the same interface, flags and addresses for
/// mDNS's group, a destination the default route takes and loopback, and the same "no route"
/// for an IPv6 destination where this Mac has no unscoped IPv6 default (the silent lookup's
/// answer carries `ESRCH`, the plain one's write fails with it). Where it has one, both find it.
///
/// NEGATIVE CONTROL (2026-11-12, on a Mac with no unscoped IPv6 default): with `route_get_as`
/// reading a refused answer as `io::Error::other(errno)` in place of the errno itself, the
/// silent lookup's "no route" is no word of the table and this fails for the IPv6 destination
/// (`left: None, right: Some(None)`).
#[test]
fn either_type_of_lookup_reads_the_same_route() {
    for to in [
        IpAddr::V4(Ipv4Addr::new(224, 0, 0, 251)),
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(documentation_address()),
    ] {
        let (_, silent) = ask(to, RTM_GET_SILENT);
        let (_, plain) = ask(to, RTM_GET);
        assert!(plain.is_some(), "the table was read: {to}");
        assert_eq!(silent, plain, "{to}");
    }
}

/// A kernel that refuses the silent type is asked with `RTM_GET` instead, once: the refused
/// lookup is written again and answered, and every later lookup starts with `RTM_GET`. The
/// stand-in kernel refuses the first write (`EINVAL`, `EPERM`, `EOPNOTSUPP`) and the real one
/// answers the second. A destination no route takes (`ESRCH`) is the kernel's answer, written
/// once and latching nothing, and a lookup refused in both types was refused for itself.
///
/// NEGATIVE CONTROL (2026-11-12): with `ESRCH` among `silent_refused`'s errnos, this fails at
/// "no route is an answer, not a refusal of the type" (`left: [17, 4], right: [17]`).
#[test]
fn a_kernel_that_refuses_the_silent_type_is_asked_with_rtm_get_from_then_on() {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    let to = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let (_, plain) = ask(to, RTM_GET);
    assert!(matches!(plain, Some(Some(_))), "loopback's route");
    let refusal = |errno: i32| std::io::Error::from_raw_os_error(errno);

    for errno in [libc::EINVAL, libc::EPERM, libc::EOPNOTSUPP] {
        let refused = AtomicBool::new(false);
        let written = std::cell::RefCell::new(Vec::new());
        // The lookup, through a kernel that refuses the silent type with `errno`.
        let lookup = || {
            let asked = route_get_as(
                to,
                None,
                |request, write| {
                    lookup_write_latched(&refused, request, |bytes| {
                        let kind = bytes[std::mem::offset_of!(RtMsghdr, rtm_type)];
                        written.borrow_mut().push(kind);
                        if kind == RTM_GET_SILENT {
                            return Err(refusal(errno));
                        }
                        write(bytes)
                    })
                },
                found,
            );
            table_word(asked)
        };
        assert_eq!(lookup(), plain, "the refused lookup is still answered");
        assert_eq!(*written.borrow(), [RTM_GET_SILENT, RTM_GET], "{errno}");
        assert!(refused.load(Relaxed), "{errno} latches RTM_GET");
        assert_eq!(lookup(), plain);
        assert_eq!(
            *written.borrow(),
            [RTM_GET_SILENT, RTM_GET, RTM_GET],
            "a latched process asks with RTM_GET alone"
        );
    }

    let kind_of = |bytes: &[u8]| bytes[std::mem::offset_of!(RtMsghdr, rtm_type)];
    let refused = AtomicBool::new(false);
    let mut written = Vec::new();
    let outcome = lookup_write_latched(&refused, &mut route_get_request(to, None), |bytes| {
        written.push(kind_of(bytes));
        Err(refusal(libc::ESRCH))
    });
    assert_eq!(outcome.unwrap_err().raw_os_error(), Some(libc::ESRCH));
    assert_eq!(
        written,
        [RTM_GET_SILENT],
        "no route is an answer, not a refusal of the type"
    );
    assert!(!refused.load(Relaxed), "and latches nothing");

    let mut written = Vec::new();
    let outcome = lookup_write_latched(&refused, &mut route_get_request(to, None), |bytes| {
        written.push(kind_of(bytes));
        Err(refusal(libc::EINVAL))
    });
    assert_eq!(outcome.unwrap_err().raw_os_error(), Some(libc::EINVAL));
    assert_eq!(written, [RTM_GET_SILENT, RTM_GET]);
    assert!(
        !refused.load(Relaxed),
        "a lookup refused in both types says nothing of the silent one"
    );

    // The kernel's own "no route" in place of a refusal on the second write still latches: the
    // kernel took the type RTM_GET and refused only the silent one.
    let mut written = Vec::new();
    let outcome = lookup_write_latched(&refused, &mut route_get_request(to, None), |bytes| {
        written.push(kind_of(bytes));
        Err(refusal(if kind_of(bytes) == RTM_GET_SILENT {
            libc::EPERM
        } else {
            libc::ESRCH
        }))
    });
    assert_eq!(outcome.unwrap_err().raw_os_error(), Some(libc::ESRCH));
    assert_eq!(written, [RTM_GET_SILENT, RTM_GET]);
    assert!(refused.load(Relaxed));
}

/// [`lookup_write`] is the one place a lookup of the process gets its number and its type, so
/// this crate's lookups and aterm-net's, which write through it alike, never share a number: a
/// lookup as built carries neither; the bytes written carry the number the writer returns, both
/// writes of a fallen-back lookup the same one, differing in their type alone; each lookup's
/// number is a new one of the process's one counter, which rises strictly between draws made
/// beside it; a live ask is read by the number its bytes carried; and a message too short to
/// carry a number is written nowhere.
///
/// MUTANT (each named by the assert it fails): `lookup_write_latched` leaving `rtm_seq` as built
/// (`"the bytes carry the number returned"`); drawing its number from a counter of its own in
/// place of [`lookup_sequence`] (`"one counter"`); `route_get_as` reading the answer by a number
/// other than the one `send` returned (`"read by the number it was written as"`, after the
/// one-minute hang detector).
#[test]
fn every_lookup_is_numbered_and_typed_by_the_one_writer() {
    use std::sync::atomic::AtomicBool;
    let type_at = std::mem::offset_of!(RtMsghdr, rtm_type);
    let number_of = |bytes: &[u8]| head_i32(bytes, std::mem::offset_of!(RtMsghdr, rtm_seq));
    let built = route_get_request(Ipv4Addr::LOCALHOST.into(), None);
    assert_eq!((built[type_at], number_of(&built)), (0, 0), "as built");

    let mut written = Vec::new();
    let number = lookup_write_latched(&AtomicBool::new(false), &mut built.clone(), |bytes| {
        written.push(bytes.to_vec());
        if bytes[type_at] == RTM_GET_SILENT {
            return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
        }
        Ok(())
    })
    .expect("the plain write");
    assert_eq!(
        written
            .iter()
            .map(|bytes| (bytes[type_at], number_of(bytes)))
            .collect::<Vec<_>>(),
        [(RTM_GET_SILENT, number), (RTM_GET, number)],
        "the bytes carry the number returned"
    );
    let (silent, plain) = (&written[0], &written[1]);
    assert_eq!(
        (&silent[..type_at], &silent[type_at + 1..]),
        (&plain[..type_at], &plain[type_at + 1..]),
        "the two writes differ in their type alone"
    );
    let at = std::mem::offset_of!(RtMsghdr, rtm_seq);
    let mut unnumbered = plain.clone();
    unnumbered[type_at] = 0;
    unnumbered[at..at + 4].fill(0);
    assert_eq!(unnumbered, built, "and are otherwise the lookup as built");

    let mut taken = Vec::new();
    for _ in 0..64 {
        taken.push(lookup_sequence());
        taken.push(lookup_write_as(true, &mut built.clone(), |_| Ok(())).expect("written"));
    }
    assert!(taken.is_sorted_by(|a, b| a < b), "one counter: {taken:?}");

    let (mut carried, mut returned) = (Vec::new(), 0);
    let asked = route_get_as(
        Ipv4Addr::LOCALHOST.into(),
        None,
        |request, write| {
            returned = lookup_write_as(true, request, |bytes| {
                carried.push(number_of(bytes));
                write(bytes)
            })?;
            Ok(returned)
        },
        found,
    );
    assert!(
        matches!(table_word(asked), Some(Some(_))),
        "loopback's route, read by the number it was written as"
    );
    assert_eq!(carried, [returned]);

    let mut short = vec![0u8; std::mem::size_of::<RtMsghdr>() - 1];
    let refused = lookup_write_latched(&AtomicBool::new(false), &mut short, |_| {
        panic!("a message too short for its head is written nowhere")
    });
    assert_eq!(
        refused.map_err(|error| error.kind()),
        Err(std::io::ErrorKind::InvalidInput)
    );
}

/// Asking whether a route takes an IPv6 destination is heard by nobody: after
/// [`routed_ipv6`] of an address no other lookup names, a bystander read up to a later plain
/// `RTM_GET` holds no message naming it, neither a lookup's answer nor an `RTM_MISS`. Where no
/// route takes the address (a Mac with no unscoped IPv6 default, whose answer is `Some(false)`),
/// the bare UDP connect the ask stands in front of is the control: it fails, and the bystander
/// hears the kernel's `RTM_MISS` naming the address. Where a route does take it, the ask is
/// still unheard and there is no miss to compare with.
///
/// The connect sends no packet, and toward the documentation prefix it is no use of the local
/// network. NEGATIVE CONTROL (2026-11-12, on a Mac with no unscoped IPv6 default): with
/// `routed_ipv6` asking by that connect, this fails at "the ask was heard" with the `RTM_MISS`.
#[test]
fn asking_whether_an_ipv6_destination_is_routed_makes_no_miss() {
    let bystander = Bystander::open();
    let to = documentation_address();
    let routed = routed_ipv6(to);
    assert!(routed.is_some(), "the table was read");
    let (marker, _) = ask(Ipv4Addr::LOCALHOST.into(), RTM_GET);
    let heard = bystander.heard_up_to(marker);
    assert!(
        !heard.iter().any(|message| names(message, to)),
        "the ask was heard: {:?}",
        heard
            .iter()
            .filter(|message| names(message, to))
            .map(|message| message[std::mem::offset_of!(RtMsghdr, rtm_type)])
            .collect::<Vec<_>>()
    );
    if routed == Some(false) {
        let socket = std::net::UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).unwrap();
        assert!(
            socket.connect((to, 9)).is_err(),
            "no route takes it, so the connect has none either"
        );
        let (marker, _) = ask(Ipv4Addr::LOCALHOST.into(), RTM_GET);
        let heard = bystander.heard_up_to(marker);
        assert!(
            heard.iter().any(|message| {
                message[std::mem::offset_of!(RtMsghdr, rtm_type)] == RTM_MISS && names(message, to)
            }),
            "the connect's miss reaches a second routing socket"
        );
    }
}
