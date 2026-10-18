// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! The event loops' wait: a wake descriptor any thread may poke, and one
//! `poll(2)` over it and a backend's own descriptors with a deadline. On X11 it
//! replaces calloop's `Ping` and `EventLoop::dispatch`
//! (docs/AUDIT-third-party-2026-10-15.md, LW.7); it grew from the headless
//! loop's wake pipe and deadline rounding, which the headless loop now uses
//! from here.

use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::time::Duration;

/// An eventfd: [`Waker::wake`] adds one from any thread, the loop polls it
/// and [`Waker::drain`]s it. Non-blocking both ways: once the counter is set
/// the loop is already going to wake, so a write that would block (the
/// counter at its ceiling) is a redundant wake, not a lost one, and a read of
/// a zero counter is "nothing posted".
#[derive(Clone, Debug)]
pub(crate) struct Waker {
    fd: Arc<OwnedFd>,
}

impl Waker {
    pub(crate) fn new() -> io::Result<Waker> {
        // SAFETY: eventfd(2) takes an initial value and flags, and returns a
        // new descriptor or -1.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a descriptor eventfd just created, owned here alone.
        Ok(Waker { fd: Arc::new(unsafe { OwnedFd::from_raw_fd(fd) }) })
    }

    /// Wake the loop. Never blocks; see the type.
    pub(crate) fn wake(&self) {
        let one = 1u64.to_ne_bytes();
        // SAFETY: eight bytes from a live local to an owned descriptor; an
        // eventfd takes exactly a u64.
        unsafe { libc::write(self.fd.as_raw_fd(), one.as_ptr().cast(), one.len()) };
    }

    /// Take every pending wake.
    pub(crate) fn drain(&self) {
        let mut count = [0u8; 8];
        // SAFETY: eight bytes into a live local from an owned descriptor. One
        // read zeroes the counter; EAGAIN means it already was.
        unsafe { libc::read(self.fd.as_raw_fd(), count.as_mut_ptr().cast(), count.len()) };
    }

    pub(crate) fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

/// `pollfd` for `fd`, waiting until it is readable.
pub(crate) fn readable(fd: RawFd) -> libc::pollfd {
    libc::pollfd { fd, events: libc::POLLIN, revents: 0 }
}

/// Whether `pfd` came back with anything set (readable, hung up or in error).
pub(crate) fn ready(pfd: &libc::pollfd) -> bool {
    pfd.revents != 0
}

/// Wait until one of `fds` is ready or `timeout` elapses (`None` waits for
/// ever). A signal cutting the wait short is an early return like any other
/// wake: the caller works out what is due afresh.
pub(crate) fn wait(fds: &mut [libc::pollfd], timeout: Option<Duration>) -> io::Result<()> {
    let count = libc::nfds_t::try_from(fds.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many descriptors"))?;
    // SAFETY: `fds` is a live, exclusively borrowed slice of `count` pollfds.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), count, timeout_millis(timeout)) };
    if rc < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

/// `poll`'s timeout: whole milliseconds, rounded UP, so a wait never ends
/// before its deadline (a 119.6 ms remainder truncated to 119 wakes early and
/// costs a whole spurious turn). `None` is -1, no deadline; zero stays zero,
/// so a busy loop gains no millisecond.
pub(crate) fn timeout_millis(timeout: Option<Duration>) -> libc::c_int {
    match timeout {
        None => -1,
        Some(d) if d.is_zero() => 0,
        Some(d) => {
            let ceil_ms = d.as_nanos().div_ceil(1_000_000).max(1);
            libc::c_int::try_from(ceil_ms).unwrap_or(libc::c_int::MAX)
        },
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn a_deadline_rounds_up_to_the_millisecond() {
        assert_eq!(timeout_millis(None), -1);
        assert_eq!(timeout_millis(Some(Duration::ZERO)), 0);
        assert_eq!(timeout_millis(Some(Duration::from_nanos(1))), 1);
        assert_eq!(timeout_millis(Some(Duration::from_micros(119_600))), 120);
        assert_eq!(timeout_millis(Some(Duration::from_millis(5))), 5);
        assert_eq!(timeout_millis(Some(Duration::MAX)), libc::c_int::MAX);
    }

    /// A wake from another thread ends a wait with no deadline; the wake is
    /// gone once drained, so the next zero-length wait finds nothing.
    #[test]
    fn a_wake_from_another_thread_ends_the_wait_and_drains() {
        let waker = Waker::new().expect("eventfd");
        let remote = waker.clone();
        let poke = std::thread::spawn(move || remote.wake());
        let mut fds = [readable(waker.as_raw_fd())];
        let until = Instant::now() + Duration::from_secs(60);
        while !ready(&fds[0]) {
            assert!(Instant::now() < until, "the wake never arrived");
            wait(&mut fds, Some(Duration::from_secs(60))).expect("poll");
        }
        poke.join().expect("the waking thread");
        waker.drain();
        let mut fds = [readable(waker.as_raw_fd())];
        wait(&mut fds, Some(Duration::ZERO)).expect("poll");
        assert!(!ready(&fds[0]), "a drained wake fired again");
    }

    /// Many wakes before the loop looks are one wake, and none blocks.
    #[test]
    fn repeated_wakes_coalesce() {
        let waker = Waker::new().expect("eventfd");
        for _ in 0..1000 {
            waker.wake();
        }
        let mut fds = [readable(waker.as_raw_fd())];
        wait(&mut fds, Some(Duration::ZERO)).expect("poll");
        assert!(ready(&fds[0]));
        waker.drain();
        let mut fds = [readable(waker.as_raw_fd())];
        wait(&mut fds, Some(Duration::ZERO)).expect("poll");
        assert!(!ready(&fds[0]));
    }
}
