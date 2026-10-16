// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `rustix::fs`: the file status flags, memfd and seals.

use std::os::fd::{AsFd, AsRawFd, OwnedFd};

use libc::{c_int, c_long, c_uint};

use crate::io::{self, check, check_fd};
use crate::path;

flags! {
    /// `O_*` file status and access-mode flags (`open`, `F_GETFL`/`F_SETFL`).
    pub struct OFlags: c_uint {
        /// `O_RDONLY` (zero: never "contained", as upstream's)
        const RDONLY = libc::O_RDONLY as c_uint;
        /// `O_WRONLY`
        const WRONLY = libc::O_WRONLY as c_uint;
        /// `O_RDWR`
        const RDWR = libc::O_RDWR as c_uint;
        /// `O_ACCMODE`
        const ACCMODE = libc::O_ACCMODE as c_uint;
        /// `O_APPEND`
        const APPEND = libc::O_APPEND as c_uint;
        /// `O_CREAT`
        const CREATE = libc::O_CREAT as c_uint;
        /// `O_EXCL`
        const EXCL = libc::O_EXCL as c_uint;
        /// `O_TRUNC`
        const TRUNC = libc::O_TRUNC as c_uint;
        /// `O_NOCTTY`
        const NOCTTY = libc::O_NOCTTY as c_uint;
        /// `O_NONBLOCK`
        const NONBLOCK = libc::O_NONBLOCK as c_uint;
        /// `O_CLOEXEC`
        const CLOEXEC = libc::O_CLOEXEC as c_uint;
        /// `O_DIRECTORY`
        const DIRECTORY = libc::O_DIRECTORY as c_uint;
        /// `O_NOFOLLOW`
        const NOFOLLOW = libc::O_NOFOLLOW as c_uint;
        /// `O_NOATIME`
        const NOATIME = libc::O_NOATIME as c_uint;
        /// `O_DIRECT`
        const DIRECT = libc::O_DIRECT as c_uint;
        /// `O_PATH`
        const PATH = libc::O_PATH as c_uint;
    }
}

flags! {
    /// `S_I*` permission bits.
    pub struct Mode: libc::mode_t {
        /// `S_IRWXU`
        const RWXU = libc::S_IRWXU;
        /// `S_IRUSR`
        const RUSR = libc::S_IRUSR;
        /// `S_IWUSR`
        const WUSR = libc::S_IWUSR;
        /// `S_IXUSR`
        const XUSR = libc::S_IXUSR;
        /// `S_IRWXG`
        const RWXG = libc::S_IRWXG;
        /// `S_IRGRP`
        const RGRP = libc::S_IRGRP;
        /// `S_IWGRP`
        const WGRP = libc::S_IWGRP;
        /// `S_IXGRP`
        const XGRP = libc::S_IXGRP;
        /// `S_IRWXO`
        const RWXO = libc::S_IRWXO;
        /// `S_IROTH`
        const ROTH = libc::S_IROTH;
        /// `S_IWOTH`
        const WOTH = libc::S_IWOTH;
        /// `S_IXOTH`
        const XOTH = libc::S_IXOTH;
        /// `S_ISUID`
        const SUID = libc::S_ISUID;
        /// `S_ISGID`
        const SGID = libc::S_ISGID;
        /// `S_ISVTX`
        const SVTX = libc::S_ISVTX;
    }
}

flags! {
    /// `MFD_*` flags for [`memfd_create`].
    pub struct MemfdFlags: c_uint {
        /// `MFD_CLOEXEC`
        const CLOEXEC = libc::MFD_CLOEXEC;
        /// `MFD_ALLOW_SEALING`
        const ALLOW_SEALING = libc::MFD_ALLOW_SEALING;
        /// `MFD_HUGETLB`
        const HUGETLB = libc::MFD_HUGETLB;
    }
}

flags! {
    /// `F_SEAL_*` flags for [`fcntl_add_seals`].
    pub struct SealFlags: u32 {
        /// `F_SEAL_SEAL`
        const SEAL = libc::F_SEAL_SEAL as u32;
        /// `F_SEAL_SHRINK`
        const SHRINK = libc::F_SEAL_SHRINK as u32;
        /// `F_SEAL_GROW`
        const GROW = libc::F_SEAL_GROW as u32;
        /// `F_SEAL_WRITE`
        const WRITE = libc::F_SEAL_WRITE as u32;
        /// `F_SEAL_FUTURE_WRITE`
        const FUTURE_WRITE = libc::F_SEAL_FUTURE_WRITE as u32;
    }
}

/// `fcntl(fd, F_GETFL)`: the file status and access-mode flags.
#[inline]
#[doc(alias = "F_GETFL")]
pub fn fcntl_getfl<Fd: AsFd>(fd: Fd) -> io::Result<OFlags> {
    // SAFETY: F_GETFL takes no argument.
    let ret = check(unsafe { libc::fcntl(fd.as_fd().as_raw_fd(), libc::F_GETFL) })?;
    Ok(OFlags::from_bits_retain(ret as c_uint))
}

/// `fcntl(fd, F_SETFL, flags)`: sets the file status flags. The kernel keeps
/// only the ones F_SETFL may change (append, non-blocking, direct, noatime).
#[inline]
#[doc(alias = "F_SETFL")]
pub fn fcntl_setfl<Fd: AsFd>(fd: Fd, flags: OFlags) -> io::Result<()> {
    // SAFETY: F_SETFL takes one `int`, passed by value.
    let ret = unsafe { libc::fcntl(fd.as_fd().as_raw_fd(), libc::F_SETFL, flags.bits() as c_int) };
    check(ret).map(drop)
}

/// `fcntl(fd, F_ADD_SEALS, seals)`: adds seals to a memfd.
#[inline]
#[doc(alias = "F_ADD_SEALS")]
pub fn fcntl_add_seals<Fd: AsFd>(fd: Fd, seals: SealFlags) -> io::Result<()> {
    // SAFETY: F_ADD_SEALS takes one `int`, passed by value.
    let ret = unsafe {
        libc::fcntl(
            fd.as_fd().as_raw_fd(),
            libc::F_ADD_SEALS,
            seals.bits() as c_int,
        )
    };
    check(ret).map(drop)
}

/// `memfd_create(name, flags)`: an anonymous file. Through `syscall(2)`, as
/// upstream issues it, so no glibc newer than the rest of the binary needs is
/// required (glibc's wrapper exists only since 2.27); a kernel older than 3.17
/// answers `NOSYS`.
#[inline]
pub fn memfd_create<P: path::Arg>(name: P, flags: MemfdFlags) -> io::Result<OwnedFd> {
    name.into_with_c_str(|name| {
        // SAFETY: `name` is a NUL-terminated string that outlives the call;
        // the second argument is the flags word, by value.
        let ret: c_long =
            unsafe { libc::syscall(libc::SYS_memfd_create, name.as_ptr(), flags.bits()) };
        // A descriptor or -1: both fit `int`.
        check_fd(c_int::try_from(ret).unwrap_or(-1))
    })
}
