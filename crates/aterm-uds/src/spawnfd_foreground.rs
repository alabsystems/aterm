// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Terminal custody is private to an actual owned Child, never a saved group number.

use std::{fs::File, io, os::fd::AsRawFd as _};

pub(super) struct Lease {
    terminal: File,
    owner: libc::pid_t,
    previous: libc::pid_t,
    group: libc::pid_t,
    active: bool,
}

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why)
}

impl Lease {
    pub(super) fn prepare(pid: u32, terminal: File) -> io::Result<Self> {
        let group = libc::pid_t::try_from(pid).map_err(|_| refused("child pid out of range"))?;
        // SAFETY: integer-only queries of this process and its actual unreaped child.
        let (owner, previous, session, child_session, child_group) = unsafe {
            (
                libc::getpid(),
                libc::getpgrp(),
                libc::getsid(0),
                libc::getsid(group),
                libc::getpgid(group),
            )
        };
        if group <= 1 || session < 0 || child_session != session || child_group != group {
            return Err(refused(
                "terminal handoff needs an owned self-led child in this session",
            ));
        }
        // SAFETY: the owned descriptor stays open; an unrelated or non-terminal fd refuses.
        let current = unsafe { libc::tcgetpgrp(terminal.as_raw_fd()) };
        if current < 0 {
            return Err(io::Error::last_os_error());
        }
        if current != previous {
            return Err(refused(
                "a background caller cannot take terminal foreground",
            ));
        }
        Ok(Self {
            terminal,
            owner,
            previous,
            group,
            active: false,
        })
    }

    pub(super) fn take(&mut self) -> io::Result<()> {
        // Keep the lease even if a setter/mask error leaves its effect uncertain.
        self.active = true;
        set_group(&self.terminal, self.group)
    }

    pub(super) fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        // SAFETY: queries of the current process; a fork copy or changed caller job
        // cannot restore a different owner's terminal custody.
        if unsafe { libc::getpid() != self.owner || libc::getpgrp() != self.previous } {
            return Err(refused("terminal controller identity changed"));
        }
        // SAFETY: the lease owns the open terminal descriptor.
        let current = unsafe { libc::tcgetpgrp(self.terminal.as_raw_fd()) };
        if current < 0 {
            return Err(io::Error::last_os_error());
        }
        if current == self.group {
            set_group(&self.terminal, self.previous)?;
        }
        self.active = false;
        Ok(())
    }

    /// A lost child no longer supplies the numeric group authority for restoration.
    pub(super) fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Block SIGTTOU on this thread only: returning foreground from a background
/// controller must not stop it or alter another thread's signal dispositions.
fn set_group(terminal: &File, group: libc::pid_t) -> io::Result<()> {
    struct Mask(Option<libc::sigset_t>);
    impl Mask {
        fn restore(&mut self) -> io::Result<()> {
            let Some(previous) = self.0.as_ref() else {
                return Ok(());
            };
            // SAFETY: a complete mask captured on this same thread, no retained pointer.
            let code =
                unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, previous, std::ptr::null_mut()) };
            if code == 0 {
                self.0 = None;
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(code))
            }
        }
    }
    impl Drop for Mask {
        fn drop(&mut self) {
            let _ = self.restore();
        }
    }
    let mut blocked = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
    let mut previous = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: both signal sets are writable, initialized before use, and not retained.
    let code = unsafe {
        libc::sigemptyset(blocked.as_mut_ptr());
        libc::sigaddset(blocked.as_mut_ptr(), libc::SIGTTOU);
        libc::pthread_sigmask(libc::SIG_BLOCK, blocked.as_ptr(), previous.as_mut_ptr())
    };
    if code != 0 {
        return Err(io::Error::from_raw_os_error(code));
    }
    // SAFETY: pthread_sigmask initialized the complete old mask on success.
    let mut mask = Mask(Some(unsafe { previous.assume_init() }));
    let result = loop {
        // SAFETY: integer terminal setter, descriptor owned for the call.
        if unsafe { libc::tcsetpgrp(terminal.as_raw_fd(), group) } == 0 {
            break Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            break Err(error);
        }
    };
    mask.restore()?;
    result
}
