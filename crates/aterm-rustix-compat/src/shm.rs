// Copyright 2026 Andrew Yates
// Copyright rustix contributors
// SPDX-License-Identifier: Apache-2.0
//
// `shm_path` is derived from rustix 0.38.44 src/backend/linux_raw/shm/syscalls.rs
// (`get_shm_name`; Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT, used
// here under Apache-2.0) and modified by the aterm project in 2026: it builds a
// `CString` instead of a fixed buffer, and `open`/`unlink` call aterm-libc.

//! `rustix::shm`: POSIX shared memory, as upstream's Linux backend does it:
//! the object `/dev/shm/<name>`, opened close-on-exec.

use std::ffi::{CStr, CString};
use std::os::fd::OwnedFd;

use libc::{c_int, c_uint};

pub use crate::fs::Mode;
use crate::io::{self, check, check_fd};
use crate::path;

flags! {
    /// `O_*` flags for [`open`].
    pub struct ShmOFlags: c_uint {
        /// `O_CREAT`
        const CREATE = libc::O_CREAT as c_uint;
        /// `O_EXCL`
        const EXCL = libc::O_EXCL as c_uint;
        /// `O_RDONLY`
        const RDONLY = libc::O_RDONLY as c_uint;
        /// `O_RDWR`
        const RDWR = libc::O_RDWR as c_uint;
        /// `O_TRUNC`
        const TRUNC = libc::O_TRUNC as c_uint;
    }
}

pub use ShmOFlags as OFlags;
#[doc(hidden)]
pub use open as shm_open;
#[doc(hidden)]
pub use unlink as shm_unlink;

/// The longest name upstream accepts, slashes included.
const NAME_MAX: usize = 255;
const SHM_DIR: &[u8] = b"/dev/shm/";

/// `/dev/shm/<name>`, after upstream's checks: at most `NAME_MAX` bytes
/// (`NAMETOOLONG`), and after any leading slashes a single non-empty path
/// component that is neither `.` nor `..` (`INVAL`). The leading slashes are
/// kept, as upstream keeps them.
fn shm_path(name: &CStr) -> io::Result<CString> {
    let name = name.to_bytes();
    if name.len() > NAME_MAX {
        return Err(io::Errno::NAMETOOLONG);
    }
    let after_slashes = &name[name.iter().take_while(|b| **b == b'/').count()..];
    if after_slashes.is_empty()
        || after_slashes == b"."
        || after_slashes == b".."
        || after_slashes.contains(&b'/')
    {
        return Err(io::Errno::INVAL);
    }
    let mut path = Vec::with_capacity(SHM_DIR.len() + name.len() + 1);
    path.extend_from_slice(SHM_DIR);
    path.extend_from_slice(name);
    // `name` came from a `CStr`, so it holds no NUL.
    CString::new(path).map_err(|_| io::Errno::INVAL)
}

/// `shm_open(name, flags, mode)`: opens (or creates) a shared memory object.
#[doc(alias = "shm_open")]
#[inline]
pub fn open<P: path::Arg>(name: P, flags: OFlags, mode: Mode) -> io::Result<OwnedFd> {
    name.into_with_c_str(|name| {
        let path = shm_path(name)?;
        let oflag = (flags.bits() as c_int) | libc::O_CLOEXEC;
        // SAFETY: `path` is NUL-terminated and outlives the call; the mode is
        // passed by value, as `open`'s variadic third argument.
        check_fd(unsafe { libc::open(path.as_ptr(), oflag, mode.bits()) })
    })
}

/// `shm_unlink(name)`: removes a shared memory object's name.
#[doc(alias = "shm_unlink")]
#[inline]
pub fn unlink<P: path::Arg>(name: P) -> io::Result<()> {
    name.into_with_c_str(|name| {
        let path = shm_path(name)?;
        // SAFETY: `path` is NUL-terminated and outlives the call.
        check(unsafe { libc::unlink(path.as_ptr()) }).map(drop)
    })
}
