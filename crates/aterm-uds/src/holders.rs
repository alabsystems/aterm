// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The holder scan: which live processes of this uid hold a descriptor for one
//! character device, named by its `st_rdev`.
//!
//! The PTY keeper offers a dead window's terminal to the next window only when
//! no live process still holds that terminal's master
//! (`docs/DESIGN-pty-keeper-2026-09-26.md`). A master's `st_rdev` survives
//! `dup` and `SCM_RIGHTS`, and a same-uid process cannot mint one, so the rdev
//! is the terminal's identity across processes.
//!
//! macOS reads it through libproc: `proc_listpids(PROC_UID_ONLY)`, then per
//! process `proc_pidinfo(PROC_PIDLISTFDS)` and, per vnode descriptor,
//! `proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO)`. A process whose table cannot be
//! read is reported in [`HolderScan::unreadable`], never counted as a
//! non-holder; the caller decides what that means.
//!
//! A process that has exited holds nothing: the kernel closes its descriptors
//! at the exit and answers `ESRCH`, even for a zombie the listing still names.
//! A descriptor that closed between the listing and its read answers `EBADF`
//! and holds nothing either. Neither makes a table unreadable.
//!
//! Nor does a descriptor on a DEAD VNODE: its read answers `ENOENT`, and
//! nothing is held through it. The kernel reclaims a vnode that descriptors
//! still hold when it is revoked — `revoke(2)`, or the exit of a terminal's
//! controlling process, which revokes that terminal for every process still
//! holding it (`proc_exit`, `bsd/kern/kern_exit.c`) — or when its volume is
//! force-unmounted; the vnode then names no file and no device (`vclean`:
//! its data gone, its operations deadfs's). In xnu (xnu-12377.121.6,
//! `bsd/kern/proc_info.c`), `PROC_PIDFDVNODEPATHINFO` answers `EBADF` from
//! `fp_get_ftype` (no such descriptor, or no vnode), then `pid_vnodeinfopath`
//! takes the vnode with `vnode_getwithvid`, whose `ENOENT`s
//! (`vnode_getiocount`, `bsd/vfs/vfs_subr.c`) are exactly a vnode `vclean`
//! marked `VL_DEAD` and an id that moved because it was reclaimed. What comes
//! after — the MAC and authorization checks and the filesystem's getattr
//! (`fill_vnodeinfo`, `vn_stat`) — is a live vnode's answer, and a refusal
//! there (`EPERM`, `EACCES`, any other) keeps the table unknown; the one
//! `ENOENT` it can give is a filesystem saying the object no longer exists,
//! which names no path either. Measured 2026-10-01: a test binary churning
//! PTYs left 67 such readings in 15 s, and each made its process unknown for
//! its whole life — every gone owner's build directory kept by the disk
//! watch while it ran. The path scan reads a process's working directory
//! the same way, and a dead one is no working directory (its module docs).
//!
//! Nor is a table with NO descriptor in it: libproc answers its listing with
//! 0 and `errno` left clear, where a refusal sets `errno` (`list_fds`). A
//! process that closed them all holds nothing. A test harness's process-group
//! anchor (a Python that `closerange`s every descriptor and waits) once read as
//! unknown, and kept every gone owner's build directory on the machine from the
//! disk watch while any such anchor ran.
//!
//! ONE WALK SERVES A SET OF DEVICES ([`scan_holders_of`]). The walk costs the
//! same whichever devices it looks for — every process of the uid, every vnode
//! descriptor of each — so a caller with many devices to ask about (the
//! keeper, for every master a dead window registered or a hold still waits
//! on) asks once for all of them. Asked once per device, a desk of a few
//! hundred masters cost the keeper's only thread seconds per question
//! (round eight, findings 55 and 78).
//!
//! Other platforms answer `Unsupported`.

use std::io;

/// What one scan found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HolderScan {
    /// Processes that hold at least one descriptor for the device.
    pub holders: Vec<u32>,
    /// Processes of this uid whose descriptor table could not be read: refused,
    /// or failed for a reason other than having exited, a descriptor having
    /// closed mid-read or a descriptor's vnode being dead (a process that has
    /// exited holds nothing, and is not listed here).
    pub unreadable: Vec<u32>,
    /// How many processes were scanned.
    pub scanned: usize,
}

impl HolderScan {
    /// Whether `pid` holds the device.
    #[must_use]
    pub fn holds(&self, pid: u32) -> bool {
        self.holders.contains(&pid)
    }
}

/// Scan every live process of this uid except `skip` for a descriptor whose
/// `st_rdev` is `rdev`. `skip` is the caller's own pid when it holds the device
/// itself (the keeper's custody copy).
///
/// # Errors
/// The process listing itself failing; `Unsupported` off macOS.
pub fn scan_rdev_holders(rdev: u64, skip: &[u32]) -> io::Result<HolderScan> {
    Ok(scan_holders_of(&[rdev], skip)?.pop().unwrap_or_default())
}

/// [`scan_rdev_holders`] for every device in `rdevs` at once, answered in
/// `rdevs`' order: ONE listing of the uid's processes and one read of each
/// process's descriptor table serve the whole set, so the cost is one walk
/// however many devices are asked about. Each answer is what
/// [`scan_rdev_holders`] would say of that device from the same reads. Asking
/// about no device reads nothing.
///
/// # Errors
/// The process listing itself failing; `Unsupported` off macOS.
#[cfg(target_vendor = "apple")]
// Skip: bottoms out at the libproc FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
pub fn scan_holders_of(rdevs: &[u64], skip: &[u32]) -> io::Result<Vec<HolderScan>> {
    let mut scans = vec![HolderScan::default(); rdevs.len()];
    if rdevs.is_empty() {
        return Ok(scans);
    }
    for pid in uid_pids()? {
        if skip.contains(&pid) {
            continue;
        }
        for (scan, read) in scans.iter_mut().zip(read_table(pid, rdevs)) {
            scan.scanned += 1;
            match read {
                TableRead::Holds => scan.holders.push(pid),
                TableRead::Clear | TableRead::Exited => {}
                TableRead::Unreadable => scan.unreadable.push(pid),
            }
        }
    }
    Ok(scans)
}

/// See the macOS twin.
///
/// # Errors
/// Always `Unsupported`.
#[cfg(not(target_vendor = "apple"))]
pub fn scan_holders_of(_rdevs: &[u64], _skip: &[u32]) -> io::Result<Vec<HolderScan>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the holder scan reads libproc (macOS)",
    ))
}

/// Whether the one process `pid` holds a descriptor for `rdev`: `Some(true)`,
/// `Some(false)` (also when it has exited: it holds nothing), or `None` when
/// its table could not be read.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn process_holds_rdev(pid: u32, rdev: u64) -> Option<bool> {
    match read_table(pid, &[rdev]).first() {
        Some(TableRead::Holds) => Some(true),
        Some(TableRead::Clear | TableRead::Exited) => Some(false),
        Some(TableRead::Unreadable) | None => None,
    }
}

/// What reading one process's descriptor table for one device found.
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableRead {
    /// A descriptor for the device.
    Holds,
    /// No descriptor for it.
    Clear,
    /// The process has exited (`ESRCH`), before the listing's read or during
    /// the descriptors' reads: every descriptor it had is closed.
    Exited,
    /// Unknown: the table was refused, or a read failed otherwise.
    Unreadable,
}

/// Why one libproc read failed, by its `errno`.
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failed {
    /// `ESRCH`: no such process; it has exited (a zombie's descriptors are
    /// closed at its exit) or been reaped.
    Exited,
    /// `EBADF`, reading one descriptor: closed since the listing, or its
    /// number reused by something that is no vnode.
    Closed,
    /// `ENOENT`, reading one descriptor: its vnode is dead — revoked, or its
    /// volume force-unmounted — and names no file and no device (the module
    /// docs). Reading a process's working directory, the same: a directory
    /// dead with its force-unmounted volume, which names no path (the path
    /// scan's module docs). Listing a process's table, it is no vnode's
    /// answer, and is unknown like [`Failed::Other`].
    Dead,
    /// Anything else (`EPERM`, or a failure that set no `errno`).
    Other,
}

#[cfg(target_vendor = "apple")]
impl Failed {
    const ESRCH: i32 = 3;
    const EBADF: i32 = 9;
    /// A descriptor's or a working directory's read's answer for a dead
    /// vnode, and `proc_pidpath`'s for a process whose executable has been
    /// unlinked since its exec (a rebuilt binary, a replaced app).
    const ENOENT: i32 = 2;

    fn of(errno: i32) -> Self {
        match errno {
            Self::ESRCH => Self::Exited,
            Self::EBADF => Self::Closed,
            Self::ENOENT => Self::Dead,
            _ => Self::Other,
        }
    }

    /// The failure the last libproc call on this thread reported (its
    /// `errno`, cleared before the call by [`clear_errno`]).
    fn last() -> Self {
        let failed = Self::of(errno());
        #[cfg(test)]
        let failed = hook::as_read(failed);
        failed
    }
}

/// What `pid`'s descriptor table says of each device in `rdevs`, in order,
/// from one listing and one read per vnode descriptor. The reads stop once
/// every device is found.
// Skip: bottoms out at the libproc FFI calls.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn read_table(pid: u32, rdevs: &[u64]) -> Vec<TableRead> {
    let every = |read| vec![read; rdevs.len()];
    let Ok(pid) = i32::try_from(pid) else {
        return every(TableRead::Unreadable);
    };
    let fds = match list_fds(pid) {
        Ok(fds) => fds,
        Err(Failed::Exited) => return every(TableRead::Exited),
        Err(Failed::Closed | Failed::Dead | Failed::Other) => {
            return every(TableRead::Unreadable);
        }
    };
    #[cfg(test)]
    hook::after_listing(pid, &fds);
    // Each asked device, sorted, with where its answer goes.
    let mut wanted: Vec<(u64, usize)> = rdevs.iter().copied().zip(0..).collect();
    wanted.sort_unstable();
    let mut held = vec![false; rdevs.len()];
    let mut found = 0;
    let mut unreadable = false;
    for (fd, kind) in fds {
        if kind != ffi::PROX_FDTYPE_VNODE {
            continue;
        }
        match vnode_rdev(pid, fd) {
            // `dev_t` is 32 bits on Darwin; the caller's u64 is `st_rdev` widened.
            Ok(r) => {
                let r = u64::from(r);
                let from = wanted.partition_point(|(w, _)| *w < r);
                for (_, at) in wanted.iter().skip(from).take_while(|(w, _)| *w == r) {
                    if let Some(slot) = held.get_mut(*at)
                        && !*slot
                    {
                        *slot = true;
                        found += 1;
                    }
                }
                if found == held.len() {
                    break;
                }
            }
            // It exited after the listing: none of its descriptors is open.
            Err(Failed::Exited) => return every(TableRead::Exited),
            // A descriptor closed between the listing and this read is gone,
            // and one on a dead vnode names no device: nothing is held
            // through either.
            Err(Failed::Closed | Failed::Dead) => {}
            // Any other failure makes the verdict unknown for every device
            // not found.
            Err(Failed::Other) => unreadable = true,
        }
    }
    held.into_iter()
        .map(|held| match (held, unreadable) {
            (true, _) => TableRead::Holds,
            (false, true) => TableRead::Unreadable,
            (false, false) => TableRead::Clear,
        })
        .collect()
}

/// See the macOS twin.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn process_holds_rdev(_pid: u32, _rdev: u64) -> Option<bool> {
    None
}

#[cfg(target_vendor = "apple")]
mod ffi {
    // libproc.h / sys/proc_info.h.
    pub const PROC_UID_ONLY: u32 = 4;
    pub const PROC_PIDLISTFDS: i32 = 1;
    pub const PROC_PIDFDVNODEPATHINFO: i32 = 2;
    pub const PROX_FDTYPE_VNODE: u32 = 1;
    /// `sizeof(struct proc_fdinfo)`: `int32_t proc_fd; uint32_t proc_fdtype;`.
    pub const FDINFO_SIZE: usize = 8;
    /// `sizeof(struct vnode_fdinfowithpath)`.
    pub const VNODE_PATH_SIZE: usize = 1200;
    /// `offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_rdev)`.
    pub const VST_RDEV: usize = 140;
    /// `offsetof(struct vnode_fdinfowithpath, pvip.vip_path)`: after the
    /// 24-byte `proc_fileinfo` and the 152-byte `vnode_info`.
    pub const VNODE_PATH: usize = 176;
    /// `MAXPATHLEN`: the size of every `vip_path`.
    pub const MAXPATHLEN: usize = 1024;
    /// `PROC_PIDVNODEPATHINFO`: a process's current and root directories.
    pub const PROC_PIDVNODEPATHINFO: i32 = 9;
    /// `sizeof(struct proc_vnodepathinfo)`: two `vnode_info_path`s.
    pub const VNODEPATHINFO_SIZE: usize = 2352;
    /// `offsetof(struct proc_vnodepathinfo, pvi_cdir.vip_path)`.
    pub const CDIR_PATH: usize = 152;
    /// `PROC_PIDPATHINFO_MAXSIZE`.
    pub const PIDPATH_MAX: usize = 4096;

    unsafe extern "C" {
        pub fn proc_listpids(kind: u32, typeinfo: u32, buffer: *mut i32, size: i32) -> i32;
        pub fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut core::ffi::c_void,
            size: i32,
        ) -> i32;
        pub fn proc_pidfdinfo(
            pid: i32,
            fd: i32,
            flavor: i32,
            buffer: *mut core::ffi::c_void,
            size: i32,
        ) -> i32;
        pub fn proc_pidpath(pid: i32, buffer: *mut core::ffi::c_void, size: u32) -> i32;
        pub fn geteuid() -> u32;
        /// Darwin's `errno` location.
        pub fn __error() -> *mut i32;
    }
}

/// Clear `errno`, so that a libproc call's failure reads as its own.
#[cfg(target_vendor = "apple")]
fn clear_errno() {
    // SAFETY: `__error` returns this thread's live `errno` slot.
    unsafe { *ffi::__error() = 0 };
}

/// This thread's `errno`: what the last libproc call set, 0 for none.
#[cfg(target_vendor = "apple")]
fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Every live process of this uid, by pid: the listing every scan here walks
/// (`proc_listpids(PROC_UID_ONLY)`), for a caller that reads something else
/// of each process. A zombie, or a process exiting, is still listed: what is
/// read of it says that it exited ([`exe_of`]).
///
/// # Errors
///
/// The listing failing; `Unsupported` on a platform with no reader.
#[cfg(target_vendor = "apple")]
pub fn uid_pids() -> io::Result<Vec<u32>> {
    // SAFETY: a side-effect-free getter.
    let uid = unsafe { ffi::geteuid() };
    // Size, then fill with headroom: processes start between the two calls, and
    // a full buffer is re-asked with more room rather than trusted.
    let mut room = 1024usize;
    loop {
        let mut pids = vec![0i32; room];
        let bytes = i32::try_from(room * 4).map_err(|_| io::Error::other("pid list too large"))?;
        // SAFETY: `pids` is `bytes` writable bytes of i32 slots.
        let got = unsafe { ffi::proc_listpids(ffi::PROC_UID_ONLY, uid, pids.as_mut_ptr(), bytes) };
        if got <= 0 {
            return Err(io::Error::last_os_error());
        }
        let n = usize::try_from(got).unwrap_or(0) / 4;
        if n >= room && room < (1 << 20) {
            room *= 4;
            continue;
        }
        return Ok(pids
            .into_iter()
            .take(n)
            .filter_map(|p| u32::try_from(p).ok())
            .filter(|p| *p > 0)
            .collect());
    }
}

/// See the macOS twin: `/proc`'s entries owned by this uid. One that exited
/// meanwhile is left out with the other uids'.
///
/// # Errors
///
/// `/proc` or `/proc/self` could not be read.
#[cfg(target_os = "linux")]
pub fn uid_pids() -> io::Result<Vec<u32>> {
    use std::os::unix::fs::MetadataExt as _;
    let me = std::fs::metadata("/proc/self")?.uid();
    Ok(std::fs::read_dir("/proc")?
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            (std::fs::metadata(entry.path()).ok()?.uid() == me).then_some(pid)
        })
        .collect())
}

/// See the macOS twin.
///
/// # Errors
///
/// Always `Unsupported`.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub fn uid_pids() -> io::Result<Vec<u32>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the process listing reads libproc (macOS) or /proc (Linux)",
    ))
}

/// `pid`'s descriptors, each `(fd, type)`. An EMPTY table is an answer:
/// libproc returns 0 for it and leaves `errno` clear, where any failure sets
/// `errno` — a process with no descriptor at all is read, never unknown.
#[cfg(target_vendor = "apple")]
fn list_fds(pid: i32) -> Result<Vec<(i32, u32)>, Failed> {
    clear_errno();
    // SAFETY: a NULL buffer asks for the size in bytes.
    let need = unsafe { ffi::proc_pidinfo(pid, ffi::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return Err(Failed::last());
    }
    // Headroom for descriptors opened between the two calls.
    let slots = usize::try_from(need).map_err(|_| Failed::Other)? / ffi::FDINFO_SIZE + 32;
    let mut buf = vec![0u8; slots * ffi::FDINFO_SIZE];
    let size = i32::try_from(buf.len()).map_err(|_| Failed::Other)?;
    clear_errno();
    // SAFETY: `buf` is `size` writable bytes.
    let got =
        unsafe { ffi::proc_pidinfo(pid, ffi::PROC_PIDLISTFDS, 0, buf.as_mut_ptr().cast(), size) };
    if got == 0 && errno() == 0 {
        return Ok(Vec::new());
    }
    if got <= 0 {
        return Err(Failed::last());
    }
    let got = usize::try_from(got)
        .map_err(|_| Failed::Other)?
        .min(buf.len());
    Ok(buf
        .get(..got)
        .ok_or(Failed::Other)?
        .as_chunks::<{ ffi::FDINFO_SIZE }>()
        .0
        .iter()
        .map(|c| {
            let [a, b, c0, d, e, f, g, h] = *c;
            (
                i32::from_ne_bytes([a, b, c0, d]),
                u32::from_ne_bytes([e, f, g, h]),
            )
        })
        .collect())
}

#[cfg(target_vendor = "apple")]
fn vnode_rdev(pid: i32, fd: i32) -> Result<u32, Failed> {
    let mut buf = [0u8; ffi::VNODE_PATH_SIZE];
    let size = i32::try_from(buf.len()).map_err(|_| Failed::Other)?;
    clear_errno();
    // SAFETY: `buf` is exactly the structure this flavor fills.
    let got = unsafe {
        ffi::proc_pidfdinfo(
            pid,
            fd,
            ffi::PROC_PIDFDVNODEPATHINFO,
            buf.as_mut_ptr().cast(),
            size,
        )
    };
    if got <= 0 {
        return Err(Failed::last());
    }
    if got != size {
        return Err(Failed::Other);
    }
    let bytes = buf
        .get(ffi::VST_RDEV..ffi::VST_RDEV + 4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .ok_or(Failed::Other)?;
    Ok(u32::from_ne_bytes(bytes))
}

#[path = "holders_paths.rs"]
mod paths;
pub use paths::{Exe, PathScan, PathUse, exe_of, scan_path_users, scan_path_users_of};

/// A TEST SEAM (unit tests only): run a closure between one table's listing
/// and the reads of its descriptors, to make a race certain; and read failures
/// as they were read before dead vnodes were told apart, for a negative
/// control.
#[cfg(all(test, target_vendor = "apple"))]
mod hook {
    use std::cell::{Cell, RefCell};

    use super::Failed;

    type Hook = Box<dyn FnOnce(i32, &[(i32, u32)])>;

    thread_local! {
        static AFTER_LISTING: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static DEAD_AS_UNKNOWN: Cell<bool> = const { Cell::new(false) };
    }

    /// Run `f` with this thread's failed reads classified the way they were
    /// before a dead vnode was told apart: its `ENOENT` an unknown failure.
    pub fn reading_dead_as_unknown<R>(f: impl FnOnce() -> R) -> R {
        DEAD_AS_UNKNOWN.with(|d| d.set(true));
        let out = f();
        DEAD_AS_UNKNOWN.with(|d| d.set(false));
        out
    }

    /// `failed`, as this thread classifies it now.
    pub(super) fn as_read(failed: Failed) -> Failed {
        if failed == Failed::Dead && DEAD_AS_UNKNOWN.with(Cell::get) {
            Failed::Other
        } else {
            failed
        }
    }

    /// Run `f` once, after the next listing this thread makes, with the pid
    /// listed and its `(fd, type)` pairs.
    pub fn after_next_listing(f: impl FnOnce(i32, &[(i32, u32)]) + 'static) {
        AFTER_LISTING.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    }

    pub(super) fn after_listing(pid: i32, fds: &[(i32, u32)]) {
        if let Some(f) = AFTER_LISTING.with(|h| h.borrow_mut().take()) {
            f(pid, fds);
        }
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use std::os::unix::fs::MetadataExt as _;
    use std::time::{Duration, Instant};

    use super::{ffi, hook};
    use crate::exitwatch::ExitWatch;

    const HANG: Duration = Duration::from_secs(60);

    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
        fn posix_openpt(flags: i32) -> i32;
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }

    /// The floors the descriptors below are duplicated to: under 256, macOS's
    /// default soft `RLIMIT_NOFILE` (`F_DUPFD_CLOEXEC` at or above the soft
    /// limit fails `EINVAL`), and above `pspawn`'s 200, so no sibling test's
    /// duplicate lands on either.
    const LOW: i32 = 210;
    const HIGH: i32 = 240;

    /// `fd` duplicated, close-on-exec, to the lowest free number at or above
    /// `at` (the allocator hands every other open the lowest free number, so
    /// no other thread's descriptor takes that one once it closes).
    fn dup_at(fd: &std::os::fd::OwnedFd, at: i32) -> std::os::fd::OwnedFd {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        const F_DUPFD_CLOEXEC: i32 = 67;
        // SAFETY: F_DUPFD_CLOEXEC on a descriptor we own; the result is ours.
        let high = unsafe { fcntl(fd.as_raw_fd(), F_DUPFD_CLOEXEC, at) };
        assert!(
            high >= at,
            "F_DUPFD_CLOEXEC at {at}: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: the duplicate, owned by nothing else.
        unsafe { std::os::fd::OwnedFd::from_raw_fd(high) }
    }

    /// XNU's kernel-private "redrive open" error, `EREDRIVEOPEN`: a
    /// `/dev/ptmx` open that lost the kernel's race for a pty minor to
    /// another opener, in this process or any other. aterm-pty's `open_ptmx`
    /// documents and measures it and redrives it for every pair aterm opens;
    /// this crate sits below aterm-pty, so its one test opener redrives it
    /// here.
    const EREDRIVEOPEN: i32 = -6;

    /// A fresh PTY master — a device no other process holds — at the lowest
    /// free number at or above `at` (the low original is closed), and its rdev.
    /// A lost race ([`EREDRIVEOPEN`]) is opened again; a bare open failed this
    /// module's tests with it, 3 runs in 15,000 beside a parallel opener. Any
    /// other error fails at once.
    pub(super) fn a_master_at(at: i32) -> (std::os::fd::OwnedFd, u64) {
        use std::os::fd::FromRawFd as _;
        const O_RDWR: i32 = 2;
        const O_NOCTTY: i32 = 0x20000;
        const O_CLOEXEC: i32 = 0x0100_0000;
        let started = Instant::now();
        let raw = loop {
            // Close-on-exec from the open: a sibling test's child forked
            // while the low original is open must not inherit the master.
            // SAFETY: a fresh descriptor this test owns.
            let raw = unsafe { posix_openpt(O_RDWR | O_NOCTTY | O_CLOEXEC) };
            if raw >= 0 {
                break raw;
            }
            let err = std::io::Error::last_os_error();
            assert_eq!(
                err.raw_os_error(),
                Some(EREDRIVEOPEN),
                "posix_openpt: {err}"
            );
            // A loser loses only to an opener that won, so this ends; the
            // hang detector names it if it ever does not.
            assert!(
                started.elapsed() < HANG,
                "posix_openpt lost every race for {HANG:?}"
            );
        };
        // SAFETY: as above.
        let low = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        let fd = dup_at(&low, at);
        drop(low);
        let rdev = std::fs::File::from(fd.try_clone().expect("dup"))
            .metadata()
            .expect("fstat")
            .rdev();
        (fd, rdev)
    }

    /// A descriptor closed between the listing and its read is not held and
    /// does not make the table unknown (its read answers `EBADF`). Controls: a
    /// closed descriptor at `LOW` with a second one for the same master above
    /// it still finds the device held (the scan goes on past the `EBADF`), and
    /// a failure that is neither a closed descriptor nor an exit still reads
    /// as unknown.
    #[test]
    fn a_descriptor_closed_between_the_listing_and_its_read_is_not_held() {
        use std::os::fd::AsRawFd as _;
        let me = std::process::id();
        let (fd, rdev) = a_master_at(LOW);
        let n = fd.as_raw_fd();
        assert_eq!(super::process_holds_rdev(me, rdev), Some(true), "held");
        let named = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = std::rc::Rc::clone(&named);
        hook::after_next_listing(move |_, fds| {
            seen.set(fds.contains(&(n, ffi::PROX_FDTYPE_VNODE)));
            drop(fd);
        });
        assert_eq!(
            super::process_holds_rdev(me, rdev),
            Some(false),
            "closed after the listing: not held, and not unknown"
        );
        assert!(named.get(), "the listing named the descriptor it closed");

        // Control: the LOW descriptor closes after the listing; a second
        // descriptor for the same master, above it, stays open.
        let (low, rdev) = a_master_at(LOW);
        let kept = dup_at(&low, HIGH);
        let (n, k) = (low.as_raw_fd(), kept.as_raw_fd());
        let order = std::rc::Rc::new(std::cell::Cell::new(None));
        let seen = std::rc::Rc::clone(&order);
        hook::after_next_listing(move |_, fds| {
            let at = |fd| fds.iter().position(|e| *e == (fd, ffi::PROX_FDTYPE_VNODE));
            seen.set(Some((at(n), at(k))));
            drop(low);
        });
        assert_eq!(
            super::process_holds_rdev(me, rdev),
            Some(true),
            "the closed descriptor's EBADF ends only its own read"
        );
        match order.get() {
            Some((Some(closed), Some(open))) => assert!(
                closed < open,
                "the scan meets the closed descriptor before the open one"
            ),
            other => panic!("the listing named both descriptors: {other:?}"),
        }
        drop(kept);

        // Control: only an exit, a closed descriptor or a dead vnode is a
        // known answer (a refusal, EACCES or EPERM, is not).
        use super::Failed;
        assert_eq!(Failed::of(3), Failed::Exited, "ESRCH");
        assert_eq!(Failed::of(9), Failed::Closed, "EBADF");
        assert_eq!(Failed::of(2), Failed::Dead, "ENOENT");
        for other in [0, 1, 12, 13, 22] {
            assert_eq!(Failed::of(other), Failed::Other, "errno {other}");
        }
    }

    /// A `sleep` whose stdin is `/dev/null`, once the kernel shows it holding
    /// the device; and that device's rdev. `spawnfd`'s: it holds its stdio and
    /// nothing else of this process, not another test thread's pipe for as
    /// long as it lives.
    fn a_holder_of_dev_null() -> (crate::spawnfd::Child, u64) {
        let rdev = std::fs::metadata("/dev/null").expect("stat").rdev();
        let child = crate::spawnfd::command("/bin/sleep")
            .arg("600")
            .stdin(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn");
        let started = Instant::now();
        while super::process_holds_rdev(child.id(), rdev) != Some(true) {
            assert!(
                started.elapsed() < HANG,
                "the child never showed the device"
            );
            std::thread::yield_now();
        }
        (child, rdev)
    }

    /// SIGKILL `pid` and return once the kernel reports its exit: it is a
    /// zombie then, until its parent (this test) reaps it.
    pub(super) fn kill_unreaped(pid: u32) {
        let watch = ExitWatch::watch(pid).expect("watch the child");
        // SAFETY: SIGKILL to a child this test started and has not reaped.
        unsafe { kill(pid as i32, 9) };
        let started = Instant::now();
        while watch.exit_status().is_none() {
            assert!(started.elapsed() < HANG, "the child never exited");
            std::thread::yield_now();
        }
    }

    /// An exited process holds nothing: a killed, unreaped child (a zombie in
    /// the listing) is a non-holder, never unreadable. Control: a table the
    /// kernel refuses (`EPERM`, e.g. launchd's) is still unreadable.
    #[test]
    fn an_exited_process_still_listed_holds_nothing() {
        let (mut child, rdev) = a_holder_of_dev_null();
        let pid = child.id();
        kill_unreaped(pid);
        assert!(
            super::uid_pids().expect("listing").contains(&pid),
            "the zombie is still listed: the scan meets it"
        );
        assert_eq!(super::process_holds_rdev(pid, rdev), Some(false));
        let scan = super::scan_rdev_holders(rdev, &[]).expect("scan");
        assert!(!scan.holds(pid), "{scan:?}");
        assert!(!scan.unreadable.contains(&pid), "{scan:?}");
        let _ = child.wait();
        assert_eq!(
            super::process_holds_rdev(1, rdev),
            None,
            "a refused table is unknown"
        );
    }

    /// A process that exits between the listing and the reads of its
    /// descriptors holds nothing: its descriptor reads answer `ESRCH`.
    #[test]
    fn a_process_that_exits_after_its_listing_holds_nothing() {
        let (mut child, rdev) = a_holder_of_dev_null();
        let pid = child.id();
        let listed = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let seen = std::rc::Rc::clone(&listed);
        hook::after_next_listing(move |at, fds| {
            assert_eq!(at, pid as i32);
            seen.set(
                fds.iter()
                    .filter(|(_, kind)| *kind == ffi::PROX_FDTYPE_VNODE)
                    .count(),
            );
            kill_unreaped(pid);
        });
        assert_eq!(super::process_holds_rdev(pid, rdev), Some(false));
        assert!(
            listed.get() > 0,
            "the listing named its vnodes, read after its exit"
        );
        let _ = child.wait();
    }

    /// A child holding `/dev/null` (a character device with a real rdev,
    /// unlike a regular file) is found by its rdev, and a scan that skips that
    /// child does not report it.
    #[test]
    fn the_scan_finds_a_child_holding_a_character_device() {
        let rdev = std::fs::metadata("/dev/null").expect("stat").rdev();
        assert_ne!(rdev, 0, "/dev/null is a character device");
        let mut child = crate::spawnfd::command("/bin/sleep")
            .arg("600")
            .stdin(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn");
        // The child's stdin is /dev/null from its first instruction; wait for
        // the exec to have happened by asking the kernel for the child's table
        // until it names the device (a hang detector bounds it).
        let started = std::time::Instant::now();
        loop {
            if super::process_holds_rdev(child.id(), rdev) == Some(true) {
                break;
            }
            assert!(
                started.elapsed().as_secs() < 60,
                "the child never showed the device"
            );
            std::thread::yield_now();
        }
        let scan = super::scan_rdev_holders(rdev, &[]).expect("scan");
        assert!(scan.holds(child.id()), "{scan:?}");
        assert!(scan.scanned > 0);
        let skipped = super::scan_rdev_holders(rdev, &[child.id()]).expect("scan");
        assert!(!skipped.holds(child.id()), "skip is honoured");
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            super::process_holds_rdev(child.id(), rdev),
            Some(false),
            "gone: holds nothing"
        );
    }

    /// ONE WALK, EVERY DEVICE (round eight, findings 55 and 78): asked about
    /// several devices at once, each answer is the one a scan for that device
    /// alone gives — a child holding `/dev/null`, this process holding a fresh
    /// PTY master (asked twice), a device nobody holds — in the order asked;
    /// `skip` is honoured for all of them, and asking about no device answers
    /// nothing. NEGATIVE CONTROL: each holder is found for its own device only.
    #[test]
    fn one_walk_answers_for_every_device_asked() {
        let me = std::process::id();
        let (mut child, null) = a_holder_of_dev_null();
        let (master, pty) = a_master_at(220);
        let nobody = 0xfeed_0002;
        let asked = [null, pty, nobody, pty];
        let scans = super::scan_holders_of(&asked, &[]).expect("scan");
        assert_eq!(scans.len(), asked.len());
        assert!(scans[0].holds(child.id()), "{:?}", scans[0]);
        assert!(scans[1].holds(me) && scans[3].holds(me), "{scans:?}");
        assert!(
            !scans[1].holds(child.id()),
            "the child holds no PTY of ours"
        );
        assert!(scans[2].holders.is_empty(), "{:?}", scans[2]);
        for (rdev, scan) in asked.iter().zip(&scans) {
            let alone = super::scan_rdev_holders(*rdev, &[]).expect("scan");
            // Whether THIS process holds `/dev/null` at a given moment turns
            // on its sibling tests (a `Stdio::null()` spawn opens it here for
            // a moment), never on the walk: the child answers for it.
            let pids = if *rdev == null {
                vec![child.id()]
            } else {
                vec![me, child.id()]
            };
            for pid in pids {
                assert_eq!(
                    scan.holds(pid),
                    alone.holds(pid),
                    "rdev {rdev:#x}, pid {pid}"
                );
            }
        }
        let skipped = super::scan_holders_of(&asked, &[me, child.id()]).expect("scan");
        assert!(
            skipped.iter().all(|s| !s.holds(me) && !s.holds(child.id())),
            "{skipped:?}"
        );
        assert!(
            super::scan_holders_of(&[], &[])
                .expect("nothing")
                .is_empty()
        );
        drop(master);
        let _ = child.kill();
        let _ = child.wait();
    }

    /// An rdev nobody holds is held by nobody.
    #[test]
    fn an_unheld_rdev_has_no_holders() {
        let scan = super::scan_rdev_holders(0xfeed_0001, &[]).expect("scan");
        assert!(scan.holders.is_empty(), "{scan:?}");
    }
}
