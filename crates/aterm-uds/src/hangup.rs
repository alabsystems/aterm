// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control stream peer hangup detection ([`peer_state`], [`PeerState`], [`peer_closed`]).
//! Distinguishes [`PeerState::Gone`] and [`PeerState::Idle`], and shuts sockets via [`hang_up`].

use crate::CtlStream;

/// What [`peer_state`] finds at the far end of a connected stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerState {
    /// Open both ways, with nothing queued to read.
    Idle,
    /// A read would return at once with bytes or with an error other than end-of-file.
    Readable,
    /// The peer shut its write half and nothing is queued: a read returns end-of-file.
    HalfClosed,
    /// The peer closed both halves: nothing written can ever be read, whatever is still queued.
    Gone,
}

/// The far end of `stream` (see [`PeerState`]).
#[must_use]
#[cfg(unix)]
pub fn peer_state(stream: &CtlStream) -> PeerState {
    use std::os::fd::AsRawFd;
    fd_peer_state(stream.as_raw_fd())
}

/// [`peer_state`] for a borrowed raw descriptor ([`fd_peer_closed`]).
#[must_use]
#[cfg(unix)]
pub fn fd_peer_state(fd: std::os::fd::RawFd) -> PeerState {
    if fd_peer_closed(fd) {
        return PeerState::Gone;
    }
    still_there(fd_peek(fd))
}

/// What one peek of a peer [`fd_peer_closed`] found still there means.
#[cfg(unix)]
#[inline]
const fn still_there(peek: Result<usize, std::io::ErrorKind>) -> PeerState {
    match peek {
        Ok(0) => PeerState::HalfClosed,
        Err(std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => PeerState::Idle,
        Ok(_) | Err(_) => PeerState::Readable,
    }
}

/// Whether `stream`'s peer has closed both halves ([`PeerState::Gone`]).
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

/// Shut down `stream` so its peer reads EOF and blocked local readers wake ([`Halves`]).
pub fn hang_up(stream: &(impl Halves + ?Sized)) -> bool {
    let wrote = stream.shut(std::net::Shutdown::Write).is_ok();
    let read = stream.shut(std::net::Shutdown::Read).is_ok();
    wrote || read
}

/// A connected stream socket [`hang_up`] shuts half by half ([`CtlStream`] or TCP).
pub trait Halves {
    /// `shutdown(2)` of the halves `how` names, on the socket every copy of this end shares.
    ///
    /// # Errors
    /// The socket's own: on Darwin `ENOTCONN` for a half already shut, or a socket whose peer
    /// closed entirely.
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

/// [`peer_closed`] for a borrowed raw descriptor ([`fd_peer_state`], [`PeerState::Gone`]).
#[must_use]
#[cfg(unix)]
pub fn fd_peer_closed(fd: std::os::fd::RawFd) -> bool {
    poll_now(fd, POLLOUT) & (POLLHUP | POLLERR | POLLNVAL) != 0
}

#[cfg(unix)]
const POLLOUT: i16 = 0x4;
#[cfg(unix)]
const POLLERR: i16 = 0x8;
#[cfg(unix)]
const POLLHUP: i16 = 0x10;
#[cfg(unix)]
const POLLNVAL: i16 = 0x20;

/// The `revents` one zero-timeout `poll` of `fd` for `events` reports now.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn poll_now(fd: std::os::fd::RawFd, events: i16) -> i16 {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
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
    // SAFETY: `probe` is one initialized pollfd that outlives the call, and a zero timeout never
    // parks. A bad fd is reported in `revents`, not UB.
    let ready = unsafe { poll(&mut probe, 1, 0) };
    if ready > 0 { probe.revents } else { 0 }
}

/// One `recv(MSG_PEEK | MSG_DONTWAIT)` of a byte from `fd`.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn fd_peek(fd: std::os::fd::RawFd) -> Result<usize, std::io::ErrorKind> {
    use core::ffi::{c_int, c_void};
    unsafe extern "C" {
        fn recv(socket: c_int, buffer: *mut c_void, length: usize, flags: c_int) -> isize;
    }
    const MSG_PEEK: c_int = 0x2;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const MSG_DONTWAIT: c_int = 0x40;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const MSG_DONTWAIT: c_int = 0x80;
    let mut byte = 0_u8;
    // SAFETY: the buffer is one owned byte that outlives the call, and `MSG_PEEK | MSG_DONTWAIT`
    // neither consumes nor parks. A bad fd is an error return, not UB.
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
    /// (`EPIPE`).
    #[test]
    fn a_hang_up_wakes_a_writer_blocked_on_a_tcp_peer_that_sent_its_fin() {
        use std::os::fd::AsRawFd as _;
        const HANG: std::time::Duration = std::time::Duration::from_secs(60);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let peer =
            std::net::TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (near, _) = listener.accept().expect("accept");
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

    /// The four answers, with the request policy beside each: only Gone is a hangup.
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
