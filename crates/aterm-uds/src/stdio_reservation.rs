// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admit opaque retained descriptors above stdio without moving another owner's cached number.

use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::sync::{Mutex, MutexGuard};

static ADMISSION: Mutex<()> = Mutex::new(());

/// One serialized probe, including its temporary input files before reservation begins.
/// Existing stdio ownership is left unchanged; another admission cannot borrow this
/// scope's temporary files or reservations and outlive their owners.
#[must_use = "keep admission alive through probe input files and reservation retirement"]
pub(crate) struct Admission {
    held: MutexGuard<'static, ()>,
}

impl Admission {
    pub(crate) fn enter() -> Self {
        Self::enter_with(|| {
            ADMISSION
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        })
    }

    fn enter_with(lock: impl FnOnce() -> MutexGuard<'static, ()>) -> Self {
        Self { held: lock() }
    }

    /// Run `action` with observed closed stdio held by directional CLOEXEC nulls.
    /// Existing descriptors are untouched. Only our reservations are released, before
    /// this admission can unlock, on success, error and unwind. The unique borrow prevents
    /// overlapping reservation actions under one admission.
    ///
    /// # Errors
    /// A flags query or null open failed, or allocation no longer selected its expected
    /// empty slot. The action is never called after a reservation refusal.
    pub(crate) fn while_reserved<T>(&mut self, action: impl FnOnce() -> T) -> io::Result<T> {
        let _admission = &self.held;
        Reservation::with(open_null, action)
    }
}

struct Reservation {
    _held: [Option<OwnedFd>; 3],
}

impl Reservation {
    fn with<T>(
        open: impl FnMut(i32) -> io::Result<OwnedFd>,
        action: impl FnOnce() -> T,
    ) -> io::Result<T> {
        let _reservation = Self::acquire(open)?;
        Ok(action())
    }

    fn acquire(mut open: impl FnMut(i32) -> io::Result<OwnedFd>) -> io::Result<Self> {
        let mut held = [None, None, None];
        for target in 0..3 {
            // SAFETY: F_GETFD reads flags only; an unused number answers EBADF.
            if unsafe { libc::fcntl(target, libc::F_GETFD) } >= 0 {
                continue;
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EBADF) {
                return Err(error);
            }
            let owned = open(target)?;
            if owned.as_raw_fd() != target {
                // Another allocator took the slot. Drop our actual returned descriptor and
                // previous reservations, never the number another owner acquired.
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!("stdio {target} changed during descriptor admission"),
                ));
            }
            held[usize::try_from(target).expect("a stdio number")] = Some(owned);
        }
        Ok(Self { _held: held })
    }
}

fn open_null(target: i32) -> io::Result<OwnedFd> {
    let access = if target == 0 {
        libc::O_RDONLY
    } else {
        libc::O_WRONLY
    };
    // SAFETY: a static C string; open returns a newly owned descriptor or -1.
    let fd = unsafe { libc::open(c"/dev/null".as_ptr(), access | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this descriptor was just acquired and has no other Rust owner.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(test)]
#[path = "stdio_reservation_tests.rs"]
mod tests;
