// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `rustix::event`: `eventfd` and `poll`.

use core::mem::{align_of, offset_of, size_of};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use libc::{c_int, c_uint};

use crate::io::{self, check, check_fd};

flags! {
    /// `EFD_*` flags for [`eventfd`].
    pub struct EventfdFlags: c_uint {
        /// `EFD_CLOEXEC`
        const CLOEXEC = libc::EFD_CLOEXEC as c_uint;
        /// `EFD_NONBLOCK`
        const NONBLOCK = libc::EFD_NONBLOCK as c_uint;
        /// `EFD_SEMAPHORE`
        const SEMAPHORE = libc::EFD_SEMAPHORE as c_uint;
    }
}

/// `eventfd(initval, flags)`: a kernel counter descriptor.
#[inline]
pub fn eventfd(initval: u32, flags: EventfdFlags) -> io::Result<OwnedFd> {
    // SAFETY: both arguments are passed by value.
    check_fd(unsafe { libc::eventfd(initval, flags.bits() as c_int) })
}

flags! {
    /// `POLL*` event bits of a [`PollFd`].
    pub struct PollFlags: u16 {
        /// `POLLIN`
        const IN = libc::POLLIN as u16;
        /// `POLLPRI`
        const PRI = libc::POLLPRI as u16;
        /// `POLLOUT`
        const OUT = libc::POLLOUT as u16;
        /// `POLLRDNORM`
        const RDNORM = libc::POLLRDNORM as u16;
        /// `POLLWRNORM`
        const WRNORM = libc::POLLWRNORM as u16;
        /// `POLLRDBAND`
        const RDBAND = libc::POLLRDBAND as u16;
        /// `POLLWRBAND`
        const WRBAND = libc::POLLWRBAND as u16;
        /// `POLLERR`
        const ERR = libc::POLLERR as u16;
        /// `POLLHUP`
        const HUP = libc::POLLHUP as u16;
        /// `POLLNVAL`
        const NVAL = libc::POLLNVAL as u16;
    }
}

/// One `struct pollfd`: a borrowed descriptor, the events asked for and the
/// events the kernel reported.
#[doc(alias = "pollfd")]
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PollFd<'fd> {
    fd: BorrowedFd<'fd>,
    events: u16,
    revents: u16,
}

// `poll` hands a `[PollFd]` to the kernel as a `[struct pollfd]`. `BorrowedFd`
// is documented to have the representation of a `RawFd`, so the two must agree
// field for field; these are decided by any compile for a Linux triple.
const _: () = {
    assert!(size_of::<PollFd<'static>>() == size_of::<libc::pollfd>());
    assert!(align_of::<PollFd<'static>>() == align_of::<libc::pollfd>());
    assert!(offset_of!(PollFd<'static>, fd) == offset_of!(libc::pollfd, fd));
    assert!(offset_of!(PollFd<'static>, events) == offset_of!(libc::pollfd, events));
    assert!(offset_of!(PollFd<'static>, revents) == offset_of!(libc::pollfd, revents));
    assert!(size_of::<BorrowedFd<'static>>() == size_of::<c_int>());
    assert!(size_of::<u16>() == size_of::<libc::c_short>());
};

impl<'fd> PollFd<'fd> {
    /// Watches `fd` for `events`.
    #[inline]
    pub fn new<Fd: AsFd>(fd: &'fd Fd, events: PollFlags) -> Self {
        Self::from_borrowed_fd(fd.as_fd(), events)
    }

    /// Watches another descriptor.
    #[inline]
    pub fn set_fd<Fd: AsFd>(&mut self, fd: &'fd Fd) {
        self.fd = fd.as_fd();
    }

    /// Forgets the reported events.
    #[inline]
    pub fn clear_revents(&mut self) {
        self.revents = 0;
    }

    /// Watches `fd` for `events`.
    #[inline]
    pub fn from_borrowed_fd(fd: BorrowedFd<'fd>, events: PollFlags) -> Self {
        Self {
            fd,
            events: events.bits(),
            revents: 0,
        }
    }

    /// The events the last [`poll`] reported.
    #[inline]
    pub fn revents(&self) -> PollFlags {
        PollFlags::from_bits_retain(self.revents)
    }
}

impl AsFd for PollFd<'_> {
    #[inline]
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// `poll(fds, timeout)`: waits up to `timeout` milliseconds (forever if
/// negative) for an event on any of `fds`, and returns how many have one.
#[inline]
pub fn poll(fds: &mut [PollFd<'_>], timeout: i32) -> io::Result<usize> {
    let nfds = libc::nfds_t::try_from(fds.len()).map_err(|_| io::Errno::INVAL)?;
    // SAFETY: `PollFd` has `struct pollfd`'s layout (asserted above), the
    // slice is live and exclusively borrowed for the call, and the kernel
    // writes only the `revents` fields.
    let ret = check(unsafe { libc::poll(fds.as_mut_ptr().cast(), nfds, timeout) })?;
    // A count no larger than `fds.len()`.
    Ok(ret as usize)
}
