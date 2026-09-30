// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether the far end of a connected control stream has gone away ENTIRELY.
//!
//! A control lane parked in a blocking verb (`await`, `turn`, `wait`, `ready`,
//! a relay) holds one of a fixed set of worker lanes until its own timeout —
//! up to ten minutes — even after the client that asked has been killed. The
//! lane has to notice the hangup to give itself back, and it has to notice it
//! WITHOUT reading: bytes a live client pipelined behind the request are the
//! next request, not a signal.
//!
//! The question is "can no byte I write ever be delivered?", which is exactly
//! the WRITE side's end-of-file. `poll(POLLOUT)` answers it on both kernels
//! aterm ships on, and answers it without confusing a half-close for a hangup:
//!
//! * a peer that only shut its WRITE half (`printf … | nc -U`, a client that
//!   sent its request and signalled EOF) leaves our write side open —
//!   `POLLOUT`, no `POLLHUP`: it still wants the answer;
//! * a peer that closed (or was SIGKILLed) takes both halves with it —
//!   `POLLHUP` (Darwin raises it from the write filter's EOF; Linux from the
//!   socket's full shutdown mask).
//!
//! Asking with `POLLIN` instead would be wrong on Darwin, which reports a
//! half-close as `POLLIN|POLLHUP`: measured 2026-09-25 on Darwin 25.6 with an
//! `AF_UNIX` pair — open `POLLOUT`→`0x4`, half-closed `POLLOUT`→`0x4`, closed
//! `POLLOUT`→`0x10`, while `POLLIN` read `0x11` for both of the last two.

use crate::CtlStream;

/// `true` when the peer of `stream` has closed its end completely, so nothing
/// written to it can ever be read. Never blocks. A half-closed peer (its write
/// side only) is NOT hung up. On Windows (afunix, no equivalent readiness
/// report) this is always `false`: a lane there keeps today's behaviour and
/// runs to its own timeout.
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
    #[cfg(any(target_os = "linux", target_os = "android"))]
    type Nfds = core::ffi::c_ulong;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
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
    use super::peer_closed;
    use std::io::Write;

    /// THE THREE STATES, and the one that must not be confused: a peer that is
    /// open, one that half-closed (still waiting for its answer), and one that
    /// is gone. Only the last is a hangup.
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
