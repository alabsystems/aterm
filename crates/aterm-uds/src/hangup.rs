// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether the far end of a connected control stream has gone away entirely.
//!
//! A control lane parked in a blocking verb (`await`, `turn`, `wait`, `ready`, a relay) would
//! otherwise hold its worker until its own timeout after its client dies. It must notice the
//! hangup without reading, since bytes a live client pipelined behind the request are the
//! next request.
//!
//! The question is whether any byte written can still be delivered, i.e. the
//! write side's end-of-file, and `poll(POLLOUT)` answers it on Darwin and
//! Linux without mistaking a half-close for a hangup:
//!
//! * a peer that only shut its write half (`printf … | nc -U`) leaves our
//!   write side open: `POLLOUT`, no `POLLHUP`; it still wants the answer;
//! * a peer that closed (or was SIGKILLed) takes both halves: `POLLHUP`.
//!
//! `POLLIN` would be wrong on Darwin, which reports a half-close as `POLLIN|POLLHUP`; with
//! `POLLOUT` an open or half-closed peer reads `0x4` and a closed one `0x10`.
//!
//! The other direction, ending a connection so its peer reads the hangup, is [`hang_up`].

use crate::CtlStream;

/// `true` when the peer of `stream` has closed its end completely, so nothing
/// written to it can ever be read. Never blocks. A half-closed peer (its write
/// side only) is not hung up. Always `false` on Windows (afunix has no
/// equivalent readiness report), where a lane runs to its own timeout.
#[must_use]
pub fn peer_closed(stream: &CtlStream) -> bool {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        fd_peer_closed(stream.as_raw_fd())
    }
    #[cfg(not(unix))]
    {
        let _ = stream;
        false
    }
}

/// Hang `stream` up for the whole socket: its peer reads end-of-file now, and a thread parked
/// reading any descriptor of this end wakes to its own.
///
/// `shutdown` acts on the socket, where closing acts on one descriptor and a socket closes only
/// with its last one. A `fork` on any thread hands the child a copy of every descriptor until
/// it execs (macOS 26.6 does not honor `FD_CLOFORK`), so a dropped end can stay open in a
/// child for seconds; a hung-up end cannot.
///
/// Each half on its own, the write half first, never `Shutdown::Both`: Darwin refuses a
/// both-ways `shutdown` (`ENOTCONN`) of a socket whose peer already half-closed toward it (a
/// client that sent its request and EOF, a relay's child that answered) and then shuts
/// nothing, so that waiting peer gets no end-of-file.
///
/// Returns whether this call shut either half. Darwin answers `false` for a socket that is not
/// connected (a listening one, or one whose peer closed entirely) or already shut both ways;
/// Linux's `shutdown(2)` succeeds on any AF_UNIX socket, so there it is `true` every time.
pub fn hang_up(stream: &CtlStream) -> bool {
    let wrote = stream.shutdown(std::net::Shutdown::Write).is_ok();
    let read = stream.shutdown(std::net::Shutdown::Read).is_ok();
    wrote || read
}

/// [`peer_closed`] for a borrowed raw descriptor: for a caller that recorded
/// the fd of the connection it is serving (a thread-local) rather than a
/// reference to the stream. The caller guarantees `fd` is open for the call.
#[must_use]
#[cfg(unix)]
// Skip: a zero-timeout `poll(2)` readiness probe (FFI, unverifiable body).
#[cfg_attr(trust_verify, trust::skip)]
pub fn fd_peer_closed(fd: std::os::fd::RawFd) -> bool {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    // `nfds_t`: `unsigned long` in glibc and musl, `unsigned int` on Darwin,
    // the BSDs and Android's bionic.
    #[cfg(target_os = "linux")]
    type Nfds = core::ffi::c_ulong;
    #[cfg(not(target_os = "linux"))]
    type Nfds = core::ffi::c_uint;
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: Nfds, timeout: i32) -> i32;
    }
    // The same four bits on Darwin, the BSDs and Linux.
    const POLLOUT: i16 = 0x4;
    const POLLERR: i16 = 0x8;
    const POLLHUP: i16 = 0x10;
    const POLLNVAL: i16 = 0x20;
    let mut probe = PollFd {
        fd,
        events: POLLOUT,
        revents: 0,
    };
    // SAFETY: `probe` is one initialized pollfd that outlives the call, and a
    // zero timeout never parks. A bad fd is reported in `revents`, not UB.
    let ready = unsafe { poll(&mut probe, 1, 0) };
    ready > 0 && probe.revents & (POLLHUP | POLLERR | POLLNVAL) != 0
}

#[cfg(all(test, unix))]
mod tests {
    use super::{hang_up, peer_closed};
    use std::io::{ErrorKind, Read, Write};

    /// What one non-blocking read of `stream` finds: `Ok(0)` is its
    /// end-of-file, `Err(WouldBlock)` an open socket with nothing to read.
    fn read_now(stream: &crate::CtlStream) -> Result<usize, ErrorKind> {
        stream.set_nonblocking(true).expect("non-blocking");
        (&*stream).read(&mut [0u8; 16]).map_err(|e| e.kind())
    }

    /// The `try_clone` stands in for the copy a forked child holds until it execs. (Darwin
    /// refuses a both-ways `shutdown` once the peer half-closed, and shuts no half.)
    #[test]
    fn a_hang_up_reaches_a_peer_that_half_closed_though_a_copy_survives() {
        let (near, far) = crate::CtlStream::pair().expect("pair");
        let forked_copy = near.try_clone().expect("a surviving copy");
        far.shutdown(std::net::Shutdown::Write)
            .expect("the peer half-closes toward `near`");
        assert!(hang_up(&near), "the write half was still open to shut");
        assert_eq!(read_now(&far), Ok(0), "the peer reads its end-of-file");
        assert_eq!(
            read_now(&forked_copy),
            Ok(0),
            "a reader of a surviving copy reads its own"
        );
        // Darwin refuses to shut a half again; Linux's shutdown(2) succeeds on
        // any connected AF_UNIX stream, shut or not (the Linux lane, 2026-10-16).
        #[cfg(target_os = "macos")]
        assert!(!hang_up(&near), "nothing is left to shut");
    }

    /// Between two open ends a hang-up shuts both halves: the peer and a surviving copy both
    /// read end-of-file, and the peer's later writes are not delivered.
    #[test]
    fn a_hang_up_shuts_both_halves_of_an_open_socket() {
        let (near, far) = crate::CtlStream::pair().expect("pair");
        let forked_copy = near.try_clone().expect("a surviving copy");
        assert!(hang_up(&near), "both halves were open");
        assert_eq!(read_now(&far), Ok(0), "the peer reads its end-of-file");
        let _ = (&far).write_all(b"late\n");
        assert_eq!(
            read_now(&forked_copy),
            Ok(0),
            "a surviving copy reads end-of-file, and nothing written after"
        );
    }

    /// A peer that is open, one that half-closed (still waiting for its
    /// answer), and one that is gone: only the last is a hangup.
    #[test]
    fn only_a_full_close_is_a_hangup() {
        let (server, client) = crate::CtlStream::pair().expect("pair");
        (&client).write_all(b"await idle 50\n").expect("request");
        assert!(!peer_closed(&server), "an open peer is not hung up");

        client
            .shutdown(std::net::Shutdown::Write)
            .expect("half-close");
        assert!(
            !peer_closed(&server),
            "a half-closed peer still reads its answer: NOT a hangup"
        );

        drop(client);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !peer_closed(&server) {
            assert!(
                std::time::Instant::now() < deadline,
                "a closed peer must read as hung up"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
