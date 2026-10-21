// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether the far end of a connected control stream is still there, and what it left queued.
//!
//! A control lane parked in a blocking verb (`await`, `turn`, `wait`, `ready`, `inbox get @<off>`,
//! a relay) would otherwise hold its worker until its own timeout after its client dies. It must
//! notice the hangup without reading, since bytes a live client pipelined behind the request are
//! the next request.
//!
//! [`peer_state`] is the one probe: it never blocks, consumes nothing and writes nothing, and
//! answers a [`PeerState`]. Two policies read it:
//!
//! * a request, whose client waits for its answer: only [`PeerState::Gone`] is hung up
//!   ([`peer_closed`]). A client that shut its write half (`printf … | nc -U`, `nc -N`, a relayed
//!   one-shot) still reads the answer, and bytes it queued are its next request;
//! * a push stream, whose client keeps its write half open and sends nothing after the
//!   acknowledgement: anything but [`PeerState::Idle`] means it is gone.
//!
//! Gone is decided first, by whether any byte written can still be delivered, i.e. by the write
//! side's end-of-file. `poll(POLLOUT)` answers that on Darwin and Linux without mistaking a
//! half-close for a hangup:
//!
//! * a peer that only shut its write half leaves our write side open: `POLLOUT`, no `POLLHUP`;
//!   it still wants the answer;
//! * a peer that closed (or was SIGKILLed) takes both halves: `POLLHUP`.
//!
//! `POLLIN` would be wrong on Darwin, which reports a half-close as `POLLIN|POLLHUP`; with
//! `POLLOUT` an open or half-closed peer reads `0x4` and a closed one `0x10`. A peer still there
//! is then told apart by one `recv(MSG_PEEK | MSG_DONTWAIT)`. Not by a zero-length `send`: on
//! Darwin `send` ignores `MSG_DONTWAIT` on an `AF_UNIX` socket.
//!
//! The other direction, ending a connection so its peer reads the hangup, is [`hang_up`].

use crate::CtlStream;

/// What [`peer_state`] finds at the far end of a connected stream, from one look that never
/// blocks, consumes nothing and writes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerState {
    /// Open both ways, with nothing queued to read.
    Idle,
    /// A read would return at once with bytes (the peer may have shut its write half behind
    /// them) or with an error other than end-of-file. The peer still reads what is written.
    Readable,
    /// The peer shut its write half and nothing is queued: a read returns end-of-file. The peer
    /// still reads what is written.
    HalfClosed,
    /// The peer closed both halves (every descriptor of its end closed, or it died): nothing
    /// written can ever be read, whatever is still queued.
    Gone,
}

/// The far end of `stream` (see [`PeerState`]). Never blocks.
#[must_use]
#[cfg(unix)]
pub fn peer_state(stream: &CtlStream) -> PeerState {
    use std::os::fd::AsRawFd;
    fd_peer_state(stream.as_raw_fd())
}

/// [`peer_state`] for a borrowed raw descriptor. The caller guarantees `fd` is open for the
/// call. Gone is exactly [`fd_peer_closed`]; only a peer still there is peeked.
#[must_use]
#[cfg(unix)]
pub fn fd_peer_state(fd: std::os::fd::RawFd) -> PeerState {
    if fd_peer_closed(fd) {
        return PeerState::Gone;
    }
    still_there(fd_peek(fd))
}

/// What one peek of a peer [`fd_peer_closed`] found still there means. `WouldBlock`, or an
/// `Interrupted` that saw nothing, is nothing queued yet.
#[cfg(unix)]
fn still_there(peek: Result<usize, std::io::ErrorKind>) -> PeerState {
    match peek {
        Ok(0) => PeerState::HalfClosed,
        Err(std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => PeerState::Idle,
        Ok(_) | Err(_) => PeerState::Readable,
    }
}

/// The request policy: `true` when the peer of `stream` has closed its end completely
/// ([`PeerState::Gone`]), so nothing written to it can ever be read. Never blocks. A peer that
/// only half-closed, or has bytes queued, is not hung up. Always `false` on Windows (afunix has
/// no equivalent readiness report), where a lane runs to its own timeout.
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
/// The same rule holds for TCP ([`Halves`]): a peer that sent its FIN has
/// shut this end's read half, and a both-ways `shutdown` then leaves a thread
/// blocked writing to that peer blocked; the write half's `shutdown` wakes it
/// (`EPIPE`).
///
/// Returns whether this call shut either half. Darwin answers `false` for a socket that is not
/// connected (a listening one, or one whose peer closed entirely) or already shut both ways;
/// Linux's `shutdown(2)` succeeds on any AF_UNIX socket, so there it is `true` every time.
pub fn hang_up(stream: &(impl Halves + ?Sized)) -> bool {
    let wrote = stream.shut(std::net::Shutdown::Write).is_ok();
    let read = stream.shut(std::net::Shutdown::Read).is_ok();
    wrote || read
}

/// A connected stream socket [`hang_up`] shuts half by half: a control
/// stream ([`CtlStream`]) or a TCP one.
pub trait Halves {
    /// `shutdown(2)` of the halves `how` names, on the socket every copy of
    /// this end shares.
    ///
    /// # Errors
    /// The socket's own: on Darwin `ENOTCONN` for a half already shut, or a
    /// socket whose peer closed entirely.
    fn shut(&self, how: std::net::Shutdown) -> std::io::Result<()>;
}

impl Halves for CtlStream {
    fn shut(&self, how: std::net::Shutdown) -> std::io::Result<()> {
        self.shutdown(how)
    }
}

impl Halves for std::net::TcpStream {
    fn shut(&self, how: std::net::Shutdown) -> std::io::Result<()> {
        self.shutdown(how)
    }
}

impl<S: Halves + ?Sized> Halves for &S {
    fn shut(&self, how: std::net::Shutdown) -> std::io::Result<()> {
        (**self).shut(how)
    }
}

/// [`peer_closed`] for a borrowed raw descriptor: for a caller that recorded the fd of the
/// connection it is serving (a thread-local) rather than a reference to the stream. The caller
/// guarantees `fd` is open for the call. It is [`fd_peer_state`]'s first step, so it holds
/// exactly when `fd_peer_state(fd)` is [`PeerState::Gone`], and costs one `poll` and no peek.
#[must_use]
#[cfg(unix)]
pub fn fd_peer_closed(fd: std::os::fd::RawFd) -> bool {
    poll_now(fd, POLLOUT) & (POLLHUP | POLLERR | POLLNVAL) != 0
}

// The same four `poll(2)` bits on Darwin, the BSDs and Linux.
#[cfg(unix)]
const POLLOUT: i16 = 0x4;
#[cfg(unix)]
const POLLERR: i16 = 0x8;
#[cfg(unix)]
const POLLHUP: i16 = 0x10;
#[cfg(unix)]
const POLLNVAL: i16 = 0x20;

/// The `revents` one zero-timeout `poll(2)` of `fd` for `events` reports now
/// (0 when nothing is ready). Never blocks; a bad fd reads as `POLLNVAL`.
#[cfg(unix)]
// Skip: a zero-timeout `poll(2)` readiness probe (FFI, unverifiable body).
#[cfg_attr(trust_verify, trust::skip)]
fn poll_now(fd: std::os::fd::RawFd, events: i16) -> i16 {
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
    let mut probe = PollFd {
        fd,
        events,
        revents: 0,
    };
    // SAFETY: `probe` is one initialized pollfd that outlives the call, and a
    // zero timeout never parks. A bad fd is reported in `revents`, not UB.
    let ready = unsafe { poll(&mut probe, 1, 0) };
    if ready > 0 { probe.revents } else { 0 }
}

/// One `recv(MSG_PEEK | MSG_DONTWAIT)` of a byte from `fd`: `Ok(0)` its end-of-file, `Ok(1)` a
/// byte, left queued, or the kind of error the call failed with. Never blocks, whatever the
/// descriptor's mode.
#[cfg(unix)]
// Skip: one `recv(2)` call (FFI, unverifiable body).
#[cfg_attr(trust_verify, trust::skip)]
fn fd_peek(fd: std::os::fd::RawFd) -> Result<usize, std::io::ErrorKind> {
    use core::ffi::{c_int, c_void};
    unsafe extern "C" {
        fn recv(socket: c_int, buffer: *mut c_void, length: usize, flags: c_int) -> isize;
    }
    // `MSG_PEEK` is 0x2 everywhere; `MSG_DONTWAIT` is 0x40 on Linux and Android, 0x80 on
    // Darwin and the BSDs.
    const MSG_PEEK: c_int = 0x2;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const MSG_DONTWAIT: c_int = 0x40;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const MSG_DONTWAIT: c_int = 0x80;
    let mut byte = 0_u8;
    // SAFETY: the buffer is one owned byte that outlives the call, and `MSG_PEEK |
    // MSG_DONTWAIT` neither consumes nor parks. A bad fd is an error return, not UB.
    let read = unsafe { recv(fd, (&raw mut byte).cast(), 1, MSG_PEEK | MSG_DONTWAIT) };
    usize::try_from(read).map_err(|_| std::io::Error::last_os_error().kind())
}

#[cfg(all(test, unix))]
mod tests {
    use super::{PeerState, hang_up, peer_closed, peer_state};
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

    /// A HANG-UP WAKES A WRITER BLOCKED ON A TCP PEER THAT SENT ITS FIN AND
    /// READS NOTHING: the write half's `shutdown` fails the blocked write
    /// (`EPIPE`). This is how the fleet listener cuts a connection and how an
    /// answering relay ends one, with a thread writing an answer the peer
    /// does not take.
    ///
    /// NEGATIVE CONTROL (macOS, asserted): the both-ways `shutdown` is refused
    /// once the peer's FIN is in, shuts nothing, and the writer stays blocked.
    /// Every wait here is the hang detector, never a latency budget.
    #[test]
    fn a_hang_up_wakes_a_writer_blocked_on_a_tcp_peer_that_sent_its_fin() {
        use std::os::fd::AsRawFd as _;
        const HANG: std::time::Duration = std::time::Duration::from_secs(60);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let peer =
            std::net::TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (near, _) = listener.accept().expect("accept");
        // The peer's FIN, in once `near` reads its end-of-file.
        peer.shutdown(std::net::Shutdown::Write)
            .expect("the peer's FIN");
        near.set_read_timeout(Some(HANG)).expect("read timeout");
        assert_eq!(
            (&near).read(&mut [0u8; 1]).map_err(|e| e.kind()),
            Ok(0),
            "the FIN is in"
        );
        let writing = near.try_clone().expect("the writer's copy");
        let (wrote_tx, wrote) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let chunk = vec![b'a'; 64 * 1024];
            let failed = loop {
                if let Err(e) = (&writing).write_all(&chunk) {
                    break e.kind();
                }
            };
            let _ = wrote_tx.send(failed);
        });
        // The writer has filled what the sockets hold and is blocked.
        let blocked = std::time::Instant::now() + HANG;
        while super::poll_now(near.as_raw_fd(), super::POLLOUT) & super::POLLOUT != 0 {
            assert!(
                std::time::Instant::now() < blocked,
                "the writer never filled the sockets"
            );
            std::thread::yield_now();
        }
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                near.shutdown(std::net::Shutdown::Both)
                    .map_err(|e| e.kind()),
                Err(ErrorKind::NotConnected),
                "Darwin refuses a both-ways shutdown of a socket whose peer sent its FIN"
            );
            assert!(
                wrote.try_recv().is_err(),
                "and that refusal shut no half: the writer is still blocked"
            );
        }
        assert!(hang_up(&near), "the write half was still open to shut");
        let failed = wrote.recv_timeout(HANG);
        assert!(failed.is_ok(), "the blocked write fails: {failed:?}");
        writer.join().expect("the writer");
        drop(peer);
    }

    /// Waits for `server`'s peer to read as gone, as a hang detector and never a budget: a
    /// socket closes with its LAST descriptor, and a sibling test's fork that lands between the
    /// pair and the drop hands its child a copy that keeps the peer open until that child execs.
    fn until_gone(server: &crate::CtlStream, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while peer_state(server) != PeerState::Gone {
            assert!(
                std::time::Instant::now() < deadline,
                "{what} must read as gone"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(peer_closed(server), "{what} is hung up to a request");
    }

    /// The four answers, with the request policy beside each: only Gone is a hangup. The probe
    /// consumes nothing and writes nothing. Negative control: a peer that queued a request and
    /// then closed is Gone, not Readable; a peek alone read its queued bytes as still there,
    /// and a parked blocking verb held its lane to its own timeout.
    #[test]
    fn the_probe_tells_idle_readable_half_closed_and_gone_apart() {
        let (server, client) = crate::CtlStream::pair().expect("pair");
        assert_eq!(peer_state(&server), PeerState::Idle, "nothing sent yet");
        assert!(!peer_closed(&server), "an open peer is not hung up");

        (&client).write_all(b"await idle 50\n").expect("request");
        assert_eq!(peer_state(&server), PeerState::Readable, "a queued request");
        assert!(!peer_closed(&server), "nor is one with a request queued");
        let mut request = [0_u8; 14];
        (&server)
            .read_exact(&mut request)
            .expect("the request is still queued");
        assert_eq!(&request, b"await idle 50\n");

        client
            .shutdown(std::net::Shutdown::Write)
            .expect("half-close");
        assert_eq!(
            peer_state(&server),
            PeerState::HalfClosed,
            "its request read, its end-of-file is next"
        );
        assert!(
            !peer_closed(&server),
            "a half-closed peer still reads its answer: NOT a hangup"
        );
        (&server).write_all(b"OK\n").expect("answer");
        let mut answer = [0_u8; 3];
        (&client)
            .read_exact(&mut answer)
            .expect("the peer reads the answer");
        assert_eq!(&answer, b"OK\n", "and the probe wrote nothing before it");
        drop(client);
        until_gone(&server, "a closed peer");

        let (server, client) = crate::CtlStream::pair().expect("pair");
        let queued: &[u8] = b"await inbox since=4\n";
        (&client).write_all(queued).expect("request");
        client
            .shutdown(std::net::Shutdown::Write)
            .expect("half-close");
        assert_eq!(
            peer_state(&server),
            PeerState::Readable,
            "a request queued ahead of the end-of-file"
        );
        drop(client);
        until_gone(&server, "a peer that queued a request and closed");
        let mut request = vec![0_u8; queued.len()];
        (&server)
            .read_exact(&mut request)
            .expect("the queued request is still read");
        assert_eq!(request, queued);

        let (server, client) = crate::CtlStream::pair().expect("pair");
        drop(client);
        until_gone(&server, "a peer closed without a half-close");
    }
}
