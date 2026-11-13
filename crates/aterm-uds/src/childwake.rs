// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pollable child-state hints, never a reap, process identity or completion fact.

use std::{
    io,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
};

/// A coalescing SIGCHLD kernel watch for this process. It may wake for another
/// child; the owner must recheck its actual Child. It changes no disposition or
/// signal mask. Register before checking child state to avoid a check/wait gap.
#[derive(Debug)]
pub struct ChildWake {
    queue: OwnedFd,
}

#[cfg(all(test, target_vendor = "apple"))]
#[path = "childwake_tests.rs"]
mod tests;

impl AsFd for ChildWake {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.queue.as_fd()
    }
}

impl ChildWake {
    /// Register a native child-state wake source. Explicitly refuses a caller
    /// that suppresses stop notifications or automatically reaps children.
    /// # Errors
    /// Unsupported kernel, incompatible SIGCHLD policy or native setup failure.
    #[cfg(target_vendor = "apple")]
    pub fn new() -> io::Result<Self> {
        use crate::exitwatch::kq;
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        let mut action = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
        // SAFETY: read this process's disposition into a live output, no change.
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), action.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful query initialized the whole action.
        let action = unsafe { action.assume_init() };
        if action.sa_sigaction != libc::SIG_DFL
            || action.sa_flags & (libc::SA_NOCLDSTOP | libc::SA_NOCLDWAIT) != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "child wake requires default retained-child policy and stop notifications",
            ));
        }
        let queue = crate::kept_out(|| -> io::Result<OwnedFd> {
            // SAFETY: kqueue creates a fresh descriptor or returns -1.
            let fd = unsafe { kq::kqueue() };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the descriptor is fresh and has one owner here.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            // SAFETY: mark this live descriptor close-on-exec, inside spawn serialization.
            if unsafe { kq::fcntl(fd.as_raw_fd(), kq::F_SETFD, kq::FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(fd)
        })?;
        let change = kq::Kevent {
            ident: libc::SIGCHLD as usize,
            filter: libc::EVFILT_SIGNAL,
            flags: kq::EV_ADD | kq::EV_ENABLE | libc::EV_CLEAR,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: one live EVFILT_SIGNAL change (EV_CLEAR), no returned event buffer.
        if unsafe {
            kq::kevent(
                queue.as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { queue })
    }

    /// No native implementation on this kernel.
    /// # Errors
    /// Always Unsupported.
    #[cfg(not(target_vendor = "apple"))]
    pub fn new() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "child wake requires Darwin",
        ))
    }

    /// Consume one coalesced hint without waiting or consuming child state.
    /// A later hint stays ready; a stream of other exits cannot trap the owner here.
    /// # Errors
    /// Kernel queue error or an unexpected notification.
    #[cfg(target_vendor = "apple")]
    pub fn drain(&self) -> io::Result<()> {
        use crate::exitwatch::kq;
        use std::os::fd::AsRawFd as _;
        loop {
            let mut event = kq::Kevent {
                ident: 0,
                filter: 0,
                flags: 0,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            let timeout = kq::Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: live event output and zero timeout, no new registrations.
            let n = unsafe {
                kq::kevent(
                    self.queue.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    &timeout,
                )
            };
            if n == 0 {
                return Ok(());
            }
            if n < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if event.flags & kq::EV_ERROR != 0
                || event.filter != libc::EVFILT_SIGNAL
                || event.ident != libc::SIGCHLD as usize
            {
                return Err(io::Error::other("unexpected child-state wake event"));
            }
            return Ok(());
        }
    }

    /// No native implementation on this kernel.
    /// # Errors
    /// Always Unsupported.
    #[cfg(not(target_vendor = "apple"))]
    pub fn drain(&self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "child wake requires Darwin",
        ))
    }
}
