// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control stream peer hangup detection ([`peer_state`](crate::hangup::peer_state), [`PeerState`](crate::hangup::PeerState), [`peer_closed`](crate::hangup::peer_closed)).
//! Distinguishes [`PeerState::Gone`](crate::hangup::PeerState::Gone) and [`PeerState::Idle`](crate::hangup::PeerState::Idle), and shuts sockets via [`hang_up`](crate::hangup::hang_up).

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
/// On Darwin a Unix socket's reader still entering `recv(2)` as this runs can sleep on until a
/// close, past its own timeout, and a peer asleep there can miss the end: a read that must see it
/// waits in `ready::for_read` first.
pub fn hang_up(stream: &(impl Halves + ?Sized)) -> bool {
    let wrote = stream.shut(std::net::Shutdown::Write).is_ok();
    let read = stream.shut(std::net::Shutdown::Read).is_ok();
    stream.wake_waiting();
    wrote || read
}

/// Reads a [`CtlStream`], each read waiting first where the connection's end cannot pass it by:
/// `ready::for_read`, on a reader nothing else ends. Windows' [`CtlStream`] wakes its own
/// parked reads, so there this is the plain read.
pub struct Awake<'a>(pub &'a CtlStream);

/// Read under an absolute deadline, including queued bytes after the peer closes.
/// The caller exclusively owns this stream's I/O for the call: Unix descriptor
/// flags are shared with duplicates and temporarily made nonblocking.
///
/// # Errors
/// The deadline expires, descriptor flags or readiness fail, or the read fails.
pub fn read_until(
    stream: &CtlStream,
    bytes: &mut [u8],
    deadline: std::time::Instant,
) -> std::io::Result<usize> {
    use std::io::{self, Read as _};
    if bytes.is_empty() {
        return Ok(0);
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsFd as _;
        crate::ready::with_nonblocking(stream.as_fd(), || {
            loop {
                crate::ready::until(stream.as_fd(), crate::ready::Interest::Read, deadline)?;
                match (&*stream).read(bytes) {
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                        ) => {}
                    result => return result,
                }
            }
        })
    }
    #[cfg(not(unix))]
    {
        let left = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or(io::ErrorKind::TimedOut)?;
        stream.set_read_timeout(Some(left))?;
        (&*stream).read(bytes)
    }
}

impl std::io::Read for Awake<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        #[cfg(unix)]
        crate::ready::for_read(self.0)?;
        let mut stream = self.0;
        stream.read(bytes)
    }
}

/// A connected stream socket [`hang_up`] shuts half by half ([`CtlStream`] or TCP).
pub trait Halves {
    /// `shutdown(2)` of the halves `how` names, on the socket every copy of this end shares.
    ///
    /// # Errors
    /// The socket's own: on Darwin `ENOTCONN` for a half already shut, or a socket whose peer
    /// closed entirely.
    fn shut(&self, how: std::net::Shutdown) -> std::io::Result<()>;

    /// End any read or write already waiting on the socket once both halves are shut: the
    /// shutdown itself does it on Unix and in Windows' [`CtlStream`], which wakes its own
    /// parked reads.
    fn wake_waiting(&self) {}
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

    /// Windows' `shutdown` ends no `recv` already waiting (only a later one fails at once),
    /// so the socket's pending I/O is cancelled, every thread's: the waiting read returns.
    #[cfg(windows)]
    fn wake_waiting(&self) {
        use std::os::windows::io::AsRawSocket as _;
        let Ok(handle) = crate::win::ffi::RawHandle::try_from(self.as_raw_socket()) else {
            return;
        };
        // SAFETY: the socket stays open across the call (`self` borrows it), and a null
        // OVERLAPPED asks for every pending request on it; nothing pending is no error here.
        let _ = unsafe { crate::win::ffi::CancelIoEx(handle, std::ptr::null_mut()) };
    }
}

impl<S: Halves + ?Sized> Halves for &S {
    fn shut(&self, how: std::net::Shutdown) -> std::io::Result<()> {
        (**self).shut(how)
    }

    fn wake_waiting(&self) {
        (**self).wake_waiting();
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
#[path = "hangup_tests.rs"]
mod tests;

/// What every platform's hang-up keeps, Windows' `shutdown` included ([`Halves::wake_waiting`]).
#[cfg(test)]
#[path = "hangup_wake_tests.rs"]
mod wake_tests;
