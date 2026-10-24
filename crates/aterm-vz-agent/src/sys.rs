// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The agent's libc calls, declared against the libc it links (musl in the guest), because
//! aterm-libc has no musl cell. `kill` is every Unix's; the rest are the Linux guest's:
//! vsock's `socket`, `bind`, `listen`, `accept4`, `connect` and `shutdown`, `clock_settime`,
//! `mount`, `umount2`, and the identity switch's `setgroups`, `setgid`, `setuid` and `chdir`.

use std::ffi::c_int;
use std::io;

unsafe extern "C" {
    fn kill(pid: c_int, signal: c_int) -> c_int;
}

/// `SIGKILL`, the same number on Linux and macOS.
pub const SIGKILL: i32 = 9;

/// A libc return value: the error it set where it is negative.
fn check(r: c_int) -> io::Result<c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

/// Send `signal` to the process group `pgid` (a command's, which leads it).
///
/// # Errors
/// `pgid` cannot be a command's group (0, 1, or past `pid_t`), or `kill` failed.
pub fn kill_group(pgid: u32, signal: i32) -> io::Result<()> {
    let pgid = c_int::try_from(pgid)
        .ok()
        .filter(|p| *p > 1)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `kill` takes two integers and reads no memory of ours.
    check(unsafe { kill(-pgid, signal) }).map(drop)
}

/// Whether process `pid` lives (`kill(pid, 0)`; one of another user's that refuses the
/// probe lives too): a stand-in's watch on the test that started it.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn alive(pid: u32) -> bool {
    const EPERM: i32 = 1;
    let Ok(pid) = c_int::try_from(pid) else {
        return false;
    };
    // SAFETY: `kill` takes two integers and reads no memory of ours; signal 0 sends none.
    let probed = unsafe { kill(pid, 0) };
    probed == 0 || io::Error::last_os_error().raw_os_error() == Some(EPERM)
}

/// End this process by SIGKILL now: a stand-in that must die abnormally (AGENTS.md).
#[cfg(any(test, debug_assertions))]
pub fn kill_self() -> ! {
    let me = std::process::id() as c_int;
    // SAFETY: `kill` takes two integers and reads no memory of ours.
    unsafe {
        kill(me, SIGKILL);
    }
    loop {
        std::thread::park();
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::{CStr, c_char, c_int, c_long, c_ulong, c_void};
    use std::io;
    use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};

    use super::check;

    /// `struct sockaddr_vm` (`<linux/vm_sockets.h>`), 16 bytes.
    #[repr(C)]
    struct SockaddrVm {
        family: u16,
        reserved1: u16,
        port: u32,
        cid: u32,
        flags: u8,
        zero: [u8; 3],
    }

    /// `struct timespec` on a 64-bit Linux (musl and glibc alike).
    #[repr(C)]
    struct Timespec {
        sec: i64,
        nsec: c_long,
    }

    const AF_VSOCK: c_int = 40;
    const SOCK_STREAM: c_int = 1;
    const SOCK_CLOEXEC: c_int = 0o2_000_000;
    const SHUT_RDWR: c_int = 2;
    const CLOCK_REALTIME: c_int = 0;
    const MS_RDONLY: c_ulong = 1;
    const MS_REMOUNT: c_ulong = 32;
    const MS_BIND: c_ulong = 4096;
    const MNT_DETACH: c_int = 2;

    /// Any CID: a listener's own.
    pub const VMADDR_CID_ANY: u32 = u32::MAX;

    unsafe extern "C" {
        fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
        fn bind(fd: c_int, addr: *const c_void, len: u32) -> c_int;
        fn listen(fd: c_int, backlog: c_int) -> c_int;
        fn accept4(fd: c_int, addr: *mut c_void, len: *mut u32, flags: c_int) -> c_int;
        fn connect(fd: c_int, addr: *const c_void, len: u32) -> c_int;
        fn shutdown(fd: c_int, how: c_int) -> c_int;
        fn clock_settime(clock: c_int, ts: *const Timespec) -> c_int;
        fn mount(
            source: *const c_char,
            target: *const c_char,
            fstype: *const c_char,
            flags: c_ulong,
            data: *const c_void,
        ) -> c_int;
        fn umount2(target: *const c_char, flags: c_int) -> c_int;
        fn setgroups(size: usize, list: *const u32) -> c_int;
        fn setgid(gid: u32) -> c_int;
        fn setuid(uid: u32) -> c_int;
        fn chdir(path: *const c_char) -> c_int;
    }

    const fn address(cid: u32, port: u32) -> SockaddrVm {
        SockaddrVm {
            family: AF_VSOCK as u16,
            reserved1: 0,
            port,
            cid,
            flags: 0,
            zero: [0; 3],
        }
    }

    const ADDRESS_LEN: u32 = std::mem::size_of::<SockaddrVm>() as u32;

    fn stream_socket() -> io::Result<OwnedFd> {
        // SAFETY: `socket` takes three integers; a descriptor it returns is ours alone.
        let fd = check(unsafe { socket(AF_VSOCK, SOCK_STREAM | SOCK_CLOEXEC, 0) })?;
        // SAFETY: `fd` was just opened and nothing else owns it.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// A vsock stream socket listening on `port` of every CID this guest has.
    ///
    /// # Errors
    /// `socket`, `bind` or `listen` failed.
    pub fn vsock_listen(port: u32) -> io::Result<OwnedFd> {
        use std::os::fd::AsRawFd as _;
        let fd = stream_socket()?;
        let addr = address(VMADDR_CID_ANY, port);
        // SAFETY: `addr` is a whole `sockaddr_vm` that outlives the call, and its length is
        // passed with it.
        check(unsafe { bind(fd.as_raw_fd(), (&raw const addr).cast(), ADDRESS_LEN) })?;
        // SAFETY: `listen` takes two integers.
        check(unsafe { listen(fd.as_raw_fd(), 64) })?;
        Ok(fd)
    }

    /// The next connection on `listener`, close-on-exec, with its peer's CID.
    ///
    /// # Errors
    /// `accept4` failed.
    pub fn vsock_accept(listener: RawFd) -> io::Result<(OwnedFd, u32)> {
        let mut peer = address(0, 0);
        let mut len = ADDRESS_LEN;
        // SAFETY: `peer` and `len` are ours, writable and outlive the call; `len` holds
        // `peer`'s size, which the kernel writes no further than.
        let fd = check(unsafe {
            accept4(listener, (&raw mut peer).cast(), &raw mut len, SOCK_CLOEXEC)
        })?;
        // SAFETY: `fd` was just accepted and nothing else owns it.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok((fd, peer.cid))
    }

    /// A vsock connection to `port` of CID `cid`.
    ///
    /// # Errors
    /// `socket` or `connect` failed.
    pub fn vsock_connect(cid: u32, port: u32) -> io::Result<OwnedFd> {
        use std::os::fd::AsRawFd as _;
        let fd = stream_socket()?;
        let addr = address(cid, port);
        // SAFETY: as `bind`'s: a whole address and its length.
        check(unsafe { connect(fd.as_raw_fd(), (&raw const addr).cast(), ADDRESS_LEN) })?;
        Ok(fd)
    }

    /// End both directions of the socket `fd`, waking every reader with EOF.
    pub fn shutdown_both(fd: RawFd) {
        // SAFETY: `shutdown` takes two integers; a failure leaves nothing to undo.
        unsafe {
            shutdown(fd, SHUT_RDWR);
        }
    }

    /// Step `CLOCK_REALTIME` to `unix_ns`.
    ///
    /// # Errors
    /// `clock_settime` failed (not root, or a time the kernel refuses).
    pub fn set_realtime(unix_ns: u64) -> io::Result<()> {
        let ts = Timespec {
            sec: i64::try_from(unix_ns / 1_000_000_000)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?,
            nsec: (unix_ns % 1_000_000_000) as c_long,
        };
        // SAFETY: `ts` is a whole `timespec` that outlives the call.
        check(unsafe { clock_settime(CLOCK_REALTIME, &raw const ts) }).map(drop)
    }

    /// Mount the virtiofs `tag` read-only at `at`.
    ///
    /// # Errors
    /// `mount` failed.
    pub fn mount_virtiofs(tag: &CStr, at: &CStr) -> io::Result<()> {
        // SAFETY: every pointer is a NUL-terminated string that outlives the call; no data.
        check(unsafe {
            mount(
                tag.as_ptr(),
                at.as_ptr(),
                c"virtiofs".as_ptr(),
                MS_RDONLY,
                std::ptr::null(),
            )
        })
        .map(drop)
    }

    /// Bind `from` at `at`.
    ///
    /// # Errors
    /// `mount` failed.
    pub fn bind_mount(from: &CStr, at: &CStr) -> io::Result<()> {
        // SAFETY: as `mount_virtiofs`'s; a bind takes no file system type.
        check(unsafe {
            mount(
                from.as_ptr(),
                at.as_ptr(),
                std::ptr::null(),
                MS_BIND,
                std::ptr::null(),
            )
        })
        .map(drop)
    }

    /// Remount the bind at `at` read-only (a bind takes no `MS_RDONLY` of its own).
    ///
    /// # Errors
    /// `mount` failed.
    pub fn remount_read_only(at: &CStr) -> io::Result<()> {
        // SAFETY: as `mount_virtiofs`'s; a remount ignores its source.
        check(unsafe {
            mount(
                c"none".as_ptr(),
                at.as_ptr(),
                std::ptr::null(),
                MS_BIND | MS_REMOUNT | MS_RDONLY,
                std::ptr::null(),
            )
        })
        .map(drop)
    }

    /// Detach the mount at `at`.
    ///
    /// # Errors
    /// `umount2` failed.
    pub fn detach(at: &CStr) -> io::Result<()> {
        // SAFETY: `at` is a NUL-terminated string that outlives the call.
        check(unsafe { umount2(at.as_ptr(), MNT_DETACH) }).map(drop)
    }

    /// Become `uid` in `groups` with primary `gid`: `setgroups`, then `setgid`, then
    /// `setuid`. It runs in a forked child before its exec, so it calls only those three
    /// async-signal-safe functions on a list built before the fork, and allocates nothing.
    ///
    /// # Errors
    /// One of the three failed: its errno.
    pub fn become_user(uid: u32, gid: u32, groups: &[u32]) -> io::Result<()> {
        // SAFETY: `groups` is a live slice and its length is passed with it.
        check(unsafe { setgroups(groups.len(), groups.as_ptr()) })?;
        // SAFETY: `setgid` and `setuid` take one integer each.
        check(unsafe { setgid(gid) })?;
        // SAFETY: as above.
        check(unsafe { setuid(uid) }).map(drop)
    }

    /// Enter `dir`. It runs in a forked child before its exec, after [`become_user`], so
    /// the directory is entered as the identity the child became; `chdir` is
    /// async-signal-safe and `dir` was built before the fork.
    ///
    /// # Errors
    /// `chdir` failed: its errno.
    pub fn change_dir(dir: &CStr) -> io::Result<()> {
        // SAFETY: `dir` is a NUL-terminated string that outlives the call.
        check(unsafe { chdir(dir.as_ptr()) }).map(drop)
    }
}
