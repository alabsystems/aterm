// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux's listening `AF_UNIX` sockets, read from the kernel's socket dump (`NETLINK_SOCK_DIAG`,
//! `sock_diag(7)`) and decoded by [`aterm_sysprobe::linux::unix_diag_listeners`]: the thin
//! reader [`super::scan_listener_holders`] matches with each process's descriptor table.

use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::time::{Duration, Instant};

use aterm_sysprobe::linux::{
    Dump, DumpFault, UnixDiagListener, unix_diag_listeners, unix_diag_request,
};

/// The sequence the one request carries, which every message of its answer repeats.
const SEQ: u32 = 1;

/// One datagram's room: the kernel sizes a dump's datagrams under 32 KiB.
const DATAGRAM_ROOM: usize = 64 * 1024;

/// Required dump completion uses a hang detector, independent of datagram count.
const DUMP_HANG: Duration = Duration::from_secs(60);

/// Every listening `AF_UNIX` socket bound at a path in this network namespace, with the file its
/// bind made, from the kernel's socket dump. The socket is connected to the kernel, so no other
/// process can send into it, and each receive waits at most `deadline`.
///
/// # Errors
///
/// The socket, the request or a receive failing (`TimedOut` past `deadline`; the kernel without
/// `CONFIG_UNIX_DIAG` or a sandbox refusing a netlink socket answers here), a datagram longer
/// than its room, a dump exceeding its minute hang detector, or a [`DumpFault`] (`InvalidData` for a
/// torn one, the errno for a refusal, `Interrupted` for an interrupted dump). `/proc/net/unix` is
/// never read: a socket name holding a newline forges its rows.
pub(super) fn unix_listeners(deadline: Duration) -> io::Result<Vec<UnixDiagListener>> {
    let started = Instant::now();
    let socket = connected()?;
    send_request(&socket)?;
    collect_dump(started, deadline, Instant::now, |room, end| {
        receive(&socket, room, end)
    })
}

fn collect_dump(
    started: Instant,
    per_receive: Duration,
    mut clock: impl FnMut() -> Instant,
    mut receive: impl FnMut(&mut [u8], Instant) -> io::Result<usize>,
) -> io::Result<Vec<UnixDiagListener>> {
    let end = started + DUMP_HANG;
    let mut room = vec![0_u8; DATAGRAM_ROOM];
    let mut listeners = Vec::new();
    loop {
        let now = clock();
        let remaining = end
            .checked_duration_since(now)
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        let n = receive(&mut room, now + per_receive.min(remaining))?;
        match unix_diag_listeners(&room[..n], SEQ, &mut listeners) {
            Ok(Dump::Done) => return Ok(listeners),
            Ok(Dump::More) => {}
            Err(fault) => return Err(fault_error(fault)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(kind: u16, seq: u32) -> Vec<u8> {
        let mut frame = vec![0; if kind == 3 { 20 } else { 16 }];
        let len = u32::try_from(frame.len()).unwrap();
        frame[..4].copy_from_slice(&len.to_ne_bytes());
        frame[4..6].copy_from_slice(&kind.to_ne_bytes());
        frame[8..12].copy_from_slice(&seq.to_ne_bytes());
        frame
    }

    #[test]
    fn complete_dump_has_no_datagram_count_ceiling() {
        let now = Instant::now();
        let mut reads = 0;
        let rows = collect_dump(
            now - Duration::from_secs(1),
            DUMP_HANG,
            || now,
            |room, end| {
                assert_eq!(end, now + Duration::from_secs(59));
                assert!(reads <= 4096, "a completed dump must stop receiving");
                let frame = message(if reads < 4096 { 1 } else { 3 }, SEQ);
                reads += 1;
                room[..frame.len()].copy_from_slice(&frame);
                Ok(frame.len())
            },
        )
        .unwrap();
        assert_eq!(reads, 4097);
        assert!(rows.is_empty());
    }

    #[test]
    fn expired_and_wrong_sequence_dumps_remain_refused() {
        let now = Instant::now();
        let error = collect_dump(
            now - Duration::from_secs(61),
            DUMP_HANG,
            || now,
            |_, _| panic!("an expired dump must not receive"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let frame = message(3, SEQ + 1);
        let error = collect_dump(
            now,
            DUMP_HANG,
            || now,
            |room, _| {
                room[..frame.len()].copy_from_slice(&frame);
                Ok(frame.len())
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn each_receive_keeps_its_caller_bound_and_original_dump_cap() {
        let now = Instant::now();
        for (age, room_left) in [(1, 2), (59, 1)] {
            let rows = collect_dump(
                now - Duration::from_secs(age),
                Duration::from_secs(2),
                || now,
                |room, end| {
                    assert_eq!(end, now + Duration::from_secs(room_left));
                    let frame = message(3, SEQ);
                    room[..frame.len()].copy_from_slice(&frame);
                    Ok(frame.len())
                },
            )
            .unwrap();
            assert!(rows.is_empty());
        }
    }
}

/// The error a [`DumpFault`] is.
fn fault_error(fault: DumpFault) -> io::Error {
    match fault {
        DumpFault::Torn(what) => io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the socket dump is torn: {what}"),
        ),
        DumpFault::Refused(errno) => io::Error::from_raw_os_error(errno),
        DumpFault::Interrupted => io::Error::new(
            io::ErrorKind::Interrupted,
            "the socket table changed while the kernel dumped it",
        ),
    }
}

/// A `NETLINK_SOCK_DIAG` socket connected to the kernel (port 0).
/// Once connected, netlink takes no unicast into it from any other port.
#[cfg_attr(trust_verify, trust::skip)]
fn connected() -> io::Result<OwnedFd> {
    // SAFETY: `socket` takes plain integers and returns a new descriptor or -1.
    let raw = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::NETLINK_SOCK_DIAG,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is the descriptor `socket` just returned, owned by nothing else.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: `sockaddr_nl` is plain integers, for which all-zero bytes are a valid value.
    let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    kernel.nl_family = libc::sa_family_t::try_from(libc::AF_NETLINK)
        .map_err(|_| io::Error::other("AF_NETLINK does not fit sa_family_t"))?;
    // SAFETY: `kernel` is a live `sockaddr_nl` of the length passed, read during the call only.
    let joined = unsafe {
        libc::connect(
            socket.as_raw_fd(),
            std::ptr::from_ref(&kernel).cast(),
            socklen_of::<libc::sockaddr_nl>(),
        )
    };
    if joined != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(socket)
}

/// `T`'s size as a `socklen_t`.
fn socklen_of<T>() -> libc::socklen_t {
    libc::socklen_t::try_from(std::mem::size_of::<T>()).unwrap_or(libc::socklen_t::MAX)
}

/// Send the dump's request whole.
#[cfg_attr(trust_verify, trust::skip)]
fn send_request(socket: &OwnedFd) -> io::Result<()> {
    let request = unix_diag_request(SEQ);
    loop {
        // SAFETY: `request` is live for the call, and its length is passed.
        let sent = unsafe {
            libc::send(
                socket.as_raw_fd(),
                request.as_ptr().cast(),
                request.len(),
                0,
            )
        };
        match usize::try_from(sent) {
            Ok(n) if n == request.len() => return Ok(()),
            Ok(_) => return Err(io::Error::other("the socket dump's request was cut short")),
            Err(_) => {
                let e = io::Error::last_os_error();
                if e.raw_os_error() != Some(libc::EINTR) {
                    return Err(e);
                }
            }
        }
    }
}

/// One datagram into `room`, its length: `TimedOut` once `end` passes, and
/// `InvalidData` for a datagram longer than `room` (`MSG_TRUNC` names its whole length).
#[cfg_attr(trust_verify, trust::skip)]
fn receive(socket: &OwnedFd, room: &mut [u8], end: Instant) -> io::Result<usize> {
    loop {
        let remaining = end
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?
            .max(Duration::from_micros(1));
        let wait = libc::timeval {
            tv_sec: libc::time_t::try_from(remaining.as_secs()).unwrap_or(libc::time_t::MAX),
            tv_usec: libc::suseconds_t::from(remaining.subsec_micros()),
        };
        // SAFETY: `wait` is a live timeval, read only during this call of its exact size.
        let set = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                std::ptr::from_ref(&wait).cast(),
                socklen_of::<libc::timeval>(),
            )
        };
        if set != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `room` is live and writable for the call, and its length is passed.
        let got = unsafe {
            libc::recv(
                socket.as_raw_fd(),
                room.as_mut_ptr().cast(),
                room.len(),
                libc::MSG_TRUNC,
            )
        };
        match usize::try_from(got) {
            Ok(n) if n <= room.len() => return Ok(n),
            Ok(n) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "a socket dump datagram of {n} bytes is longer than its {} byte room",
                        room.len()
                    ),
                ));
            }
            Err(_) => {
                let e = io::Error::last_os_error();
                match e.raw_os_error() {
                    Some(libc::EINTR) => {}
                    Some(libc::EAGAIN) => {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "the kernel's socket dump did not answer in time",
                        ));
                    }
                    _ => return Err(e),
                }
            }
        }
    }
}
