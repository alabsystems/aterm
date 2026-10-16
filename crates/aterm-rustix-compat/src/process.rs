// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `rustix::process`: the process id.

use core::num::NonZeroI32;

/// A raw process or thread id.
pub type RawPid = libc::pid_t;

/// A process or thread id: always positive.
#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub struct Pid(NonZeroI32);

impl Pid {
    /// The id of `init`, 1.
    pub const INIT: Self = match NonZeroI32::new(1) {
        Some(one) => Self(one),
        None => panic!("1 is not zero"),
    };

    /// `Some` for a positive `raw`, else `None`.
    #[inline]
    pub const fn from_raw(raw: RawPid) -> Option<Self> {
        if raw > 0 {
            match NonZeroI32::new(raw) {
                Some(id) => Some(Self(id)),
                None => None,
            }
        } else {
            None
        }
    }

    /// A `Pid` from a raw id the caller knows to be positive.
    ///
    /// # Safety
    ///
    /// `raw` must be greater than zero.
    #[inline]
    pub const unsafe fn from_raw_unchecked(raw: RawPid) -> Self {
        debug_assert!(raw > 0);
        // SAFETY: the caller guarantees `raw > 0`, so it is not zero.
        Self(unsafe { NonZeroI32::new_unchecked(raw) })
    }

    /// The id of a child process `std` spawned.
    #[inline]
    pub fn from_child(child: &std::process::Child) -> Self {
        // SAFETY: the id fork(2)/posix_spawn(3) returned to the parent for a
        // live child, which is positive.
        unsafe { Self::from_raw_unchecked(child.id() as RawPid) }
    }

    /// The id as a non-zero integer.
    #[inline]
    pub const fn as_raw_nonzero(self) -> NonZeroI32 {
        self.0
    }

    /// The raw id, or 0 for `None`.
    #[inline]
    pub fn as_raw(pid: Option<Self>) -> RawPid {
        pid.map_or(0, |pid| pid.0.get())
    }

    /// Whether this is `init`'s id.
    #[inline]
    pub const fn is_init(self) -> bool {
        self.0.get() == 1
    }
}

/// `getpid()`: this process's id.
#[inline]
pub fn getpid() -> Pid {
    // SAFETY: getpid(2) takes no arguments, cannot fail, and returns this
    // process's id, which is positive (the kernel numbers from 1).
    unsafe { Pid::from_raw_unchecked(libc::getpid()) }
}
