// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The holder scan: which live processes of this uid hold a descriptor for one character
//! device, named by its `st_rdev` ([`scan_rdev_holders`]), or for one file, named by its
//! device and inode ([`scan_file_holders`]).
//!
//! The PTY keeper offers a dead window's terminal to the next window only when no live process
//! still holds that terminal's master (`docs/DESIGN-pty-keeper-2026-09-26.md`). A master's
//! `st_rdev` survives `dup` and `SCM_RIGHTS`, and a same-uid process cannot mint one, so the
//! rdev is the terminal's identity across processes.
//!
//! macOS reads it through libproc: `proc_listpids(PROC_UID_ONLY)`, then per process
//! `proc_pidinfo(PROC_PIDLISTFDS)` and, per vnode descriptor,
//! `proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO)`. A process whose table cannot be read is reported
//! in [`HolderScan::unreadable`], never counted as a non-holder. These hold nothing and leave
//! the table readable:
//!
//! * a process that has exited (`ESRCH`), even a zombie the listing still names: the kernel
//!   closes its descriptors at the exit;
//! * a descriptor closed between the listing and its read (`EBADF`);
//! * a descriptor on a dead vnode (`ENOENT`): one revoked (`revoke(2)`, or the exit of a
//!   terminal's controlling process, which revokes it for every holder) or on a
//!   force-unmounted volume, which `vclean` leaves naming no file and no device. In xnu
//!   (xnu-12377.121.6, `bsd/kern/proc_info.c`) this flavor's `ENOENT` comes from
//!   `vnode_getwithvid` (a `VL_DEAD` vnode, or a vid the reclaim moved) or from a filesystem
//!   saying the object is gone; any later refusal (`EPERM`, `EACCES`, …) keeps the table
//!   unknown. PTY churn leaves dozens of these within seconds, and reading them as unknown
//!   would keep every gone owner's build directory from the disk watch;
//! * an empty table: libproc answers 0 with `errno` clear, where a refusal sets it
//!   (`list_fds`).
//!
//! A file's holders are the lanes' question of a lock or a conversation database before a
//! relaunch resumes it (aterm-agent's `file_holders`), which ran `lsof -t <file>`: 0.21-0.26 s
//! of wall and 0.17 s of CPU on a loaded Mac, where this walk takes 9-22 ms (measured
//! 2026-10-20 at a load of 86-96, 803 processes of this uid). Its descriptors are read without
//! their paths (`Lookup::BY_FILE`), as `lsof` reads one whose path the kernel refuses. `lsof`
//! also lists a process whose working directory, root or a mapping is the file, which a
//! holder of a lock or an open database never is without a descriptor; libproc's own
//! `proc_listpidspath`, which reads those too, walks every mapping of every process and took
//! 0.2-0.8 s.
//!
//! One walk serves a set of devices ([`scan_holders_of`]): it costs the same whichever devices
//! it looks for, so a caller with many (the keeper, for every master a dead window registered
//! or a hold still waits on) asks once for all. Asked once per device, a few hundred masters
//! cost the keeper's only thread seconds per question.
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

/// [`scan_rdev_holders`] for every device in `rdevs`, answered in `rdevs`' order, from one
/// listing of the uid's processes and one read of each table. Each answer is what
/// [`scan_rdev_holders`] would say of that device from the same reads. No device reads nothing.
///
/// # Errors
/// The process listing itself failing; `Unsupported` off macOS.
#[cfg(target_vendor = "apple")]
// Skip: bottoms out at the libproc FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
pub fn scan_holders_of(rdevs: &[u64], skip: &[u32]) -> io::Result<Vec<HolderScan>> {
    let wanted: Vec<u128> = rdevs.iter().map(|r| u128::from(*r)).collect();
    scan(&wanted, Lookup::BY_RDEV, skip)
}

/// Scan every live process of this uid except `skip` for a descriptor on the file at `path`:
/// one whose vnode has that file's device and inode (`path` followed through links, as
/// `stat` follows it). A process whose table cannot be read is in
/// [`HolderScan::unreadable`], as for a device.
///
/// # Errors
/// `path` cannot be read (`stat`), or the process listing fails; `Unsupported` off macOS.
#[cfg(target_vendor = "apple")]
pub fn scan_file_holders(path: &std::path::Path, skip: &[u32]) -> io::Result<HolderScan> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path)?;
    // `st_dev` is a 32-bit `dev_t` on Darwin, which `MetadataExt` widens with its sign;
    // `vst_dev` holds the same 32 bits.
    let dev = u32::try_from(meta.dev() & u64::from(u32::MAX)).map_err(io::Error::other)?;
    let file = Vnode {
        dev,
        ino: meta.ino(),
        rdev: 0,
    };
    Ok(scan(&[file.file_key()], Lookup::BY_FILE, skip)?
        .pop()
        .unwrap_or_default())
}

/// See the macOS twin.
///
/// # Errors
/// Always `Unsupported`.
#[cfg(not(target_vendor = "apple"))]
pub fn scan_file_holders(_path: &std::path::Path, _skip: &[u32]) -> io::Result<HolderScan> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the holder scan reads libproc (macOS)",
    ))
}

/// One walk of this uid's processes except `skip`: for each of `wanted`, in order, who holds a
/// descriptor whose vnode answers it as `lookup` reads it.
#[cfg(target_vendor = "apple")]
fn scan(wanted: &[u128], lookup: Lookup, skip: &[u32]) -> io::Result<Vec<HolderScan>> {
    let mut scans = vec![HolderScan::default(); wanted.len()];
    if wanted.is_empty() {
        return Ok(scans);
    }
    for pid in uid_pids()? {
        if skip.contains(&pid) {
            continue;
        }
        for (scan, read) in scans.iter_mut().zip(read_table(pid, wanted, lookup)) {
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

/// Whether one of `pid`'s standard descriptors (0, 1, 2) is on a dead vnode
/// (the module docs): `Some(true)`, `Some(false)` when each that is open reads
/// as a live vnode or as no vnode (a pipe, a socket), `None` when the table or
/// a descriptor could not be read, or the process has exited.
///
/// The exit of a terminal's controlling process (its session's leader)
/// revokes that terminal for every holder, so a process still holding its
/// stdio on a revoked terminal was born on a terminal whose session has
/// ended: measured on Darwin 27, the child a terminal's session forks, which
/// launchd adopts and which execs only after the leader's exit, holds 0, 1
/// and 2 on vnodes this flavor reads as `ENOENT` (the descriptors stay
/// `PROX_FDTYPE_VNODE`), and has lost `P_CONTROLT` with that exec. A
/// force-unmounted volume's file reads the same.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn stdio_on_dead_vnode(pid: u32) -> Option<bool> {
    let pid = i32::try_from(pid).ok()?;
    for (fd, kind) in list_fds(pid).ok()? {
        if !(0..=2).contains(&fd) || kind != ffi::PROX_FDTYPE_VNODE {
            continue;
        }
        // The path flavor (`Lookup::BY_RDEV`): its path lookup is what reads a
        // revoked terminal's vnode as dead (`ENOENT`).
        match vnode_of(pid, fd, Lookup::BY_RDEV) {
            Ok(_) | Err(Failed::Closed) => {}
            Err(Failed::Dead) => return Some(true),
            Err(Failed::Exited | Failed::Refused | Failed::Other) => return None,
        }
    }
    Some(false)
}

/// See the macOS twin.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn stdio_on_dead_vnode(_pid: u32) -> Option<bool> {
    None
}

/// Whether the one process `pid` holds a descriptor for `rdev`: `Some(true)`,
/// `Some(false)` (also when it has exited: it holds nothing), or `None` when
/// its table could not be read.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn process_holds_rdev(pid: u32, rdev: u64) -> Option<bool> {
    match read_table(pid, &[u128::from(rdev)], Lookup::BY_RDEV).first() {
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
    /// `ENOENT`, reading one descriptor or a working directory: its vnode is dead (revoked, or
    /// its volume force-unmounted) and names no file, device or path (the module docs).
    /// Listing a process's table, it is no vnode's answer and is unknown like
    /// [`Failed::Other`].
    Dead,
    /// `EPERM`: the kernel will not tell this process. Reading one descriptor, it refuses
    /// the vnode's metadata (a file in a protected folder, whose metadata only an entitled
    /// reader such as `lsof` is given); otherwise unknown like [`Failed::Other`].
    Refused,
    /// Anything else (`EACCES`, or a failure that set no `errno`).
    Other,
}

#[cfg(target_vendor = "apple")]
impl Failed {
    const ESRCH: i32 = 3;
    const EPERM: i32 = 1;
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
            Self::EPERM => Self::Refused,
            _ => Self::Other,
        }
    }

    /// The failure the last libproc call on this thread reported (its
    /// `errno`, cleared before the call by [`clear_errno`]).
    fn last() -> Self {
        let failed = Self::of(errno());
        #[cfg(any(test, debug_assertions))]
        let failed = hook::as_read(failed);
        failed
    }
}

/// What a vnode descriptor names: the file's device and inode, and the device a character
/// special file stands for.
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Vnode {
    /// `vst_dev`.
    dev: u32,
    /// `vst_ino`.
    ino: u64,
    /// `vst_rdev`.
    rdev: u32,
}

#[cfg(target_vendor = "apple")]
impl Vnode {
    /// The device a character special file stands for (`st_rdev`), as a scan's key.
    fn rdev_key(&self) -> u128 {
        u128::from(self.rdev)
    }

    /// The file itself (`st_dev` and `st_ino`), as a scan's key.
    fn file_key(&self) -> u128 {
        (u128::from(self.dev) << 64) | u128::from(self.ino)
    }
}

/// How a scan reads each vnode descriptor, and the key it matches the vnode by.
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy)]
struct Lookup {
    /// The `proc_pidfdinfo` flavor read.
    flavor: i32,
    /// The size of that flavor's answer.
    size: usize,
    /// The scan's key of the vnode read.
    key: fn(&Vnode) -> u128,
    /// Whether a descriptor whose vnode the kernel refuses to describe
    /// ([`Failed::Refused`]) is known not to be the one sought: true of a file
    /// this process has `stat`ed itself, which the same check let through for
    /// the same vnode, so a refused vnode is another.
    refused_is_another: bool,
}

#[cfg(target_vendor = "apple")]
impl Lookup {
    /// A device by its `st_rdev`, read with the descriptor's path
    /// (`PROC_PIDFDVNODEPATHINFO`): the keeper's read, whose path lookup also
    /// says a dead vnode is dead (`ENOENT`).
    const BY_RDEV: Self = Self {
        flavor: ffi::PROC_PIDFDVNODEPATHINFO,
        size: ffi::VNODE_PATH_SIZE,
        key: Vnode::rdev_key,
        refused_is_another: false,
    };
    /// A file by its device and inode, read without the path
    /// (`PROC_PIDFDVNODEINFO`), which the match does not need. A descriptor on
    /// a file in a protected folder is refused (`EPERM`) with or without it,
    /// where `lsof` reads it through its entitlement
    /// (`com.apple.rootless.datavault.metadata`; measured 2026-10-20: eight
    /// system agents of this uid, `lsd`'s and `dmd`'s databases among them):
    /// the file sought was `stat`ed, so such a descriptor is not on it.
    const BY_FILE: Self = Self {
        flavor: ffi::PROC_PIDFDVNODEINFO,
        size: ffi::VNODE_FD_SIZE,
        key: Vnode::file_key,
        refused_is_another: true,
    };
}

/// What `pid`'s descriptor table says of each key in `wanted`, in order, each vnode
/// descriptor read as `lookup` reads it, from one listing and one read per vnode
/// descriptor. The reads stop once every key is found.
// Skip: bottoms out at the libproc FFI calls.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn read_table(pid: u32, wanted: &[u128], lookup: Lookup) -> Vec<TableRead> {
    let every = |read| vec![read; wanted.len()];
    let Ok(pid) = i32::try_from(pid) else {
        return every(TableRead::Unreadable);
    };
    let fds = match list_fds(pid) {
        Ok(fds) => fds,
        Err(Failed::Exited) => return every(TableRead::Exited),
        Err(Failed::Closed | Failed::Dead | Failed::Refused | Failed::Other) => {
            return every(TableRead::Unreadable);
        }
    };
    #[cfg(test)]
    hook::after_listing(pid, &fds);
    // Each asked key, sorted, with where its answer goes.
    let mut wanted: Vec<(u128, usize)> = wanted.iter().copied().zip(0..).collect();
    wanted.sort_unstable();
    let mut held = vec![false; wanted.len()];
    let mut found = 0;
    let mut unreadable = false;
    for (fd, kind) in fds {
        if kind != ffi::PROX_FDTYPE_VNODE {
            continue;
        }
        match vnode_of(pid, fd, lookup) {
            // The scan's key of it: a device's `st_rdev` (32 bits on Darwin, widened),
            // or a file's device and inode.
            Ok(vnode) => {
                let r = (lookup.key)(&vnode);
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
            // A vnode whose metadata is refused is not a file this process
            // could `stat` (`Lookup::refused_is_another`).
            Err(Failed::Refused) if lookup.refused_is_another => {}
            // Any other failure makes the verdict unknown for every key
            // not found.
            Err(Failed::Refused | Failed::Other) => unreadable = true,
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

/// Every libproc constant and layout the macOS scans read by, as the C expression the SDK
/// must agree with: aterm-sysprobe's `tests/proc_info_sdk.rs` asserts each on `arm64` and
/// `x86_64`, so a header that moved a field fails there, not in a wrong holder.
#[cfg(target_vendor = "apple")]
pub const SDK_ROWS: &[(&str, i64)] = &[
    ("PROC_UID_ONLY", ffi::PROC_UID_ONLY as i64),
    ("PROC_PIDLISTFDS", ffi::PROC_PIDLISTFDS as i64),
    (
        "PROC_PIDFDVNODEPATHINFO",
        ffi::PROC_PIDFDVNODEPATHINFO as i64,
    ),
    ("PROX_FDTYPE_VNODE", ffi::PROX_FDTYPE_VNODE as i64),
    ("PROX_FDTYPE_PSHM", ffi::PROX_FDTYPE_PSHM as i64),
    ("sizeof(struct proc_fdinfo)", ffi::FDINFO_SIZE as i64),
    (
        "sizeof(struct vnode_fdinfowithpath)",
        ffi::VNODE_PATH_SIZE as i64,
    ),
    ("PROC_PIDFDVNODEINFO", ffi::PROC_PIDFDVNODEINFO as i64),
    ("sizeof(struct vnode_fdinfo)", ffi::VNODE_FD_SIZE as i64),
    (
        "offsetof(struct vnode_fdinfo, pvi.vi_stat.vst_dev)",
        ffi::VST_DEV as i64,
    ),
    (
        "offsetof(struct vnode_fdinfo, pvi.vi_stat.vst_ino)",
        ffi::VST_INO as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_dev)",
        ffi::VST_DEV as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_ino)",
        ffi::VST_INO as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_rdev)",
        ffi::VST_RDEV as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pvip.vip_path)",
        ffi::VNODE_PATH as i64,
    ),
    ("MAXPATHLEN", ffi::MAXPATHLEN as i64),
    ("PROC_PIDVNODEPATHINFO", ffi::PROC_PIDVNODEPATHINFO as i64),
    (
        "sizeof(struct proc_vnodepathinfo)",
        ffi::VNODEPATHINFO_SIZE as i64,
    ),
    (
        "offsetof(struct proc_vnodepathinfo, pvi_cdir.vip_path)",
        ffi::CDIR_PATH as i64,
    ),
    ("PROC_PIDPATHINFO_MAXSIZE", ffi::PIDPATH_MAX as i64),
    ("PROC_PIDFDPSHMINFO", ffi::PROC_PIDFDPSHMINFO as i64),
    ("sizeof(struct pshm_fdinfo)", ffi::PSHM_FDINFO_SIZE as i64),
    (
        "offsetof(struct pshm_fdinfo, pshminfo.pshm_stat.vst_size)",
        ffi::PSHM_VST_SIZE as i64,
    ),
];

#[cfg(target_vendor = "apple")]
mod ffi {
    // libproc.h / sys/proc_info.h.
    pub const PROC_UID_ONLY: u32 = 4;
    pub const PROC_PIDLISTFDS: i32 = 1;
    pub const PROC_PIDFDVNODEPATHINFO: i32 = 2;
    /// `PROC_PIDFDVNODEINFO`: a descriptor's `struct vnode_fdinfo`, its vnode without its path.
    pub const PROC_PIDFDVNODEINFO: i32 = 1;
    /// `sizeof(struct vnode_fdinfo)`: the 24-byte `proc_fileinfo` and the 152-byte
    /// `vnode_info`, as `vnode_fdinfowithpath` starts.
    pub const VNODE_FD_SIZE: usize = 176;
    pub const PROX_FDTYPE_VNODE: u32 = 1;
    /// `PROX_FDTYPE_PSHM`: a POSIX shared-memory descriptor.
    pub const PROX_FDTYPE_PSHM: u32 = 3;
    /// `sizeof(struct proc_fdinfo)`: `int32_t proc_fd; uint32_t proc_fdtype;`.
    pub const FDINFO_SIZE: usize = 8;
    /// `sizeof(struct vnode_fdinfowithpath)`.
    pub const VNODE_PATH_SIZE: usize = 1200;
    /// `offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_dev)`, and the same in
    /// `struct vnode_fdinfo` (`pvi.vi_stat.vst_dev`).
    pub const VST_DEV: usize = 24;
    /// `offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_ino)`, and the same in
    /// `struct vnode_fdinfo` (`pvi.vi_stat.vst_ino`).
    pub const VST_INO: usize = 32;
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
    /// `PROC_PIDFDPSHMINFO`: a POSIX shared-memory descriptor's `struct pshm_fdinfo`.
    pub const PROC_PIDFDPSHMINFO: i32 = 5;
    /// `sizeof(struct pshm_fdinfo)`: the 24-byte `proc_fileinfo`, then `pshm_info`.
    pub const PSHM_FDINFO_SIZE: usize = 1192;
    /// `offsetof(struct pshm_fdinfo, pshminfo.pshm_stat.vst_size)`: the object's size.
    pub const PSHM_VST_SIZE: usize = 112;

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

/// See the macOS twin. Windows: every process whose owner is this process's
/// token user, read WITHOUT opening any of them — the session manager answers
/// the listing with each process's owner SID
/// (`WTSEnumerateProcessesExW` at level 1, over `WTS_ANY_SESSION`, so a same-user
/// process in another logon session counts too, as the macOS/Linux listing does
/// across sessions). A process the manager named with no owner SID (`pUserSid`
/// null, a system process we would not open) is left out, as another uid's is.
/// This is only the LISTING: each pid is then read by the path-user scan
/// ([`scan_path_users_of`]), which opens it and classes an unreadable same-user
/// process as held.
///
/// # Errors
///
/// The token user or the process listing could not be read.
#[cfg(windows)]
pub fn uid_pids() -> io::Result<Vec<u32>> {
    win_pids::same_user_pids()
}

/// This process's token-user SID and the session manager's process listing: the
/// Windows [`uid_pids`] listing. The SID lives in a kept buffer the pointer
/// borrows; `EqualSid` needs both live at once, so the buffer is held across the
/// whole comparison.
#[cfg(windows)]
mod win_pids {
    use super::io;
    use crate::win::ffi;

    /// `TOKEN_INFORMATION_CLASS::TokenUser`.
    const TOKEN_USER: u32 = 1;
    const TOKEN_QUERY: u32 = 0x0008;
    /// `WTS_CURRENT_SERVER_HANDLE` (this server).
    const WTS_CURRENT_SERVER: ffi::RawHandle = 0;
    /// `WTS_ANY_SESSION` (`(DWORD)-2`): every session.
    const WTS_ANY_SESSION: u32 = 0xFFFF_FFFE;
    /// `WTS_PROCESS_INFO_TYPE::WTSTypeProcessInfoLevel1`.
    const WTS_PROCESS_INFO_LEVEL1: u32 = 1;

    /// This process's token-user SID, as the `TOKEN_USER` bytes that back it
    /// (the SID is a pointer into them, so they are returned and kept alive).
    fn own_sid() -> io::Result<(Vec<u8>, *const core::ffi::c_void)> {
        let mut token: ffi::RawHandle = 0;
        // SAFETY: `GetCurrentProcess` is a pseudo-handle; the out pointer is live.
        if unsafe { ffi::OpenProcessToken(ffi::GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut len: u32 = 0;
        // SAFETY: a sizing call — a null buffer asks for the length in `len`.
        unsafe { ffi::GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            // SAFETY: closes the token this function opened.
            let _ = unsafe { ffi::CloseHandle(token) };
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` is `len` writable bytes; `TOKEN_USER` fills it with a
        // `SID_AND_ATTRIBUTES` whose `Sid` points inside `buf`.
        let ok = unsafe {
            ffi::GetTokenInformation(token, TOKEN_USER, buf.as_mut_ptr().cast(), len, &mut len)
        };
        // SAFETY: closes the token this function opened.
        let _ = unsafe { ffi::CloseHandle(token) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // `TOKEN_USER { SID_AND_ATTRIBUTES User }`; `User.Sid` is the first
        // pointer-sized field, a pointer into `buf`.
        let sid = buf
            .get(..std::mem::size_of::<usize>())
            .and_then(|b| <[u8; std::mem::size_of::<usize>()]>::try_from(b).ok())
            .map(usize::from_ne_bytes)
            .filter(|p| *p != 0)
            .ok_or_else(|| io::Error::other("the token user holds no SID"))?;
        Ok((buf, sid as *const core::ffi::c_void))
    }

    /// [`super::uid_pids`]: the pids the session manager names with our owner SID.
    pub(super) fn same_user_pids() -> io::Result<Vec<u32>> {
        let (_buf, own) = own_sid()?;
        let mut level = WTS_PROCESS_INFO_LEVEL1;
        let mut info: *mut ffi::WtsProcessInfoExW = std::ptr::null_mut();
        let mut count: u32 = 0;
        // SAFETY: level and out-params are live locals; on success the manager
        // allocates `info`, freed below. We never open any listed process.
        let ok = unsafe {
            ffi::WTSEnumerateProcessesExW(
                WTS_CURRENT_SERVER,
                &mut level,
                WTS_ANY_SESSION,
                &mut info,
                &mut count,
            )
        };
        if ok == 0 || info.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut pids = Vec::new();
        // SAFETY: `info` is `count` valid `WtsProcessInfoExW`s the call wrote.
        let entries = unsafe { std::slice::from_raw_parts(info, count as usize) };
        for e in entries {
            // SAFETY: `e.pUserSid`, when non-null, is a valid SID inside the
            // manager's allocation; `own` is our live token SID. Both outlive
            // the comparison (`_buf` is still held).
            let same = !e.pUserSid.is_null()
                && unsafe { ffi::IsValidSid(e.pUserSid) } != 0
                && unsafe { ffi::EqualSid(own, e.pUserSid) } != 0;
            if same && e.ProcessId != 0 {
                pids.push(e.ProcessId);
            }
        }
        // SAFETY: frees the one allocation the enumerate call returned.
        let _ = unsafe { ffi::WTSFreeMemoryExW(WTS_PROCESS_INFO_LEVEL1, info.cast(), count) };
        Ok(pids)
    }
}

/// See the macOS twin.
///
/// # Errors
///
/// Always `Unsupported`.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
pub fn uid_pids() -> io::Result<Vec<u32>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the process listing reads libproc (macOS) or /proc (Linux)",
    ))
}

/// `pid`'s descriptors, each `(fd, type)`. An empty table is an answer: libproc returns 0 for
/// it and leaves `errno` clear, where any failure sets `errno`.
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
fn vnode_of(pid: i32, fd: i32, lookup: Lookup) -> Result<Vnode, Failed> {
    // Both flavors' answers start with the 24-byte `proc_fileinfo` and the `vnode_info`
    // the device and inode are read from; the larger holds either.
    let mut buf = [0u8; ffi::VNODE_PATH_SIZE];
    let buf = buf.get_mut(..lookup.size).ok_or(Failed::Other)?;
    let size = i32::try_from(buf.len()).map_err(|_| Failed::Other)?;
    clear_errno();
    // SAFETY: `buf` is exactly the structure `lookup.flavor` fills.
    let got = unsafe { ffi::proc_pidfdinfo(pid, fd, lookup.flavor, buf.as_mut_ptr().cast(), size) };
    if got <= 0 {
        return Err(Failed::last());
    }
    if got != size {
        return Err(Failed::Other);
    }
    let u32_at = |at: usize| {
        buf.get(at..at + 4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(u32::from_ne_bytes)
            .ok_or(Failed::Other)
    };
    let ino = buf
        .get(ffi::VST_INO..ffi::VST_INO + 8)
        .and_then(|b| <[u8; 8]>::try_from(b).ok())
        .map(u64::from_ne_bytes)
        .ok_or(Failed::Other)?;
    Ok(Vnode {
        dev: u32_at(ffi::VST_DEV)?,
        ino,
        rdev: u32_at(ffi::VST_RDEV)?,
    })
}

/// The size of the POSIX shared-memory object `fd` names in this process, or `None` when `fd` is
/// not one (a vnode, a socket, a closed number).
///
/// The PTY keeper takes a crash-restore journal region in custody only when the descriptor a
/// window hands it is shared memory of the size the window claims (the crash-restore plan §5): a
/// window could otherwise park a file, a pipe or a device beside its master, which the keeper
/// would then hand to the next window as a region to map. libproc says what a descriptor is
/// without mapping or reading it: `proc_pidfdinfo(getpid(), fd, PROC_PIDFDPSHMINFO)` fills a
/// `struct pshm_fdinfo` (1,192 bytes) for a shm descriptor and fails `EBADF` for any other, and
/// the object's size is `pshminfo.pshm_stat.vst_size` at offset 112 (both measured on macOS 27:
/// the crash-restore plan's Appendix A, and this crate's test).
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn fd_is_pshm(fd: std::os::fd::BorrowedFd<'_>) -> Option<u64> {
    use std::os::fd::AsRawFd as _;
    pshm_size(i32::try_from(std::process::id()).ok()?, fd.as_raw_fd())
}

/// The sizes of the POSIX shared-memory objects `pid`'s descriptors name, one per descriptor,
/// in descriptor order: what a process really holds of shared memory, read from the kernel's
/// table and not from the process's own bookkeeping. The PTY keeper's kernel lane asks it of the
/// keeper, so a region the keeper forgot to close shows as a descriptor still open, whatever the
/// keeper's status says (a descriptor closed between the listing and its read is left out).
///
/// # Errors
///
/// The listing failing (`pid` gone, or not this uid's).
#[cfg(target_vendor = "apple")]
pub fn pshm_sizes_of(pid: u32) -> io::Result<Vec<u64>> {
    let unreadable = || io::Error::other(format!("pid {pid}'s descriptor table is unreadable"));
    let pid = i32::try_from(pid).map_err(|_| unreadable())?;
    let fds = list_fds(pid).map_err(|_| unreadable())?;
    Ok(fds
        .into_iter()
        .filter(|(_, kind)| *kind == ffi::PROX_FDTYPE_PSHM)
        .filter_map(|(fd, _)| pshm_size(pid, fd))
        .collect())
}

/// `pid`'s descriptor `fd`'s shared-memory size, or `None` when it is no shm descriptor (or is
/// gone).
// Skip: bottoms out at the libproc FFI call.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn pshm_size(pid: i32, fd: i32) -> Option<u64> {
    let mut buf = [0u8; ffi::PSHM_FDINFO_SIZE];
    let size = i32::try_from(buf.len()).ok()?;
    // SAFETY: `buf` is exactly the structure this flavor fills; a closed `fd` fails `EBADF`.
    let got = unsafe {
        ffi::proc_pidfdinfo(
            pid,
            fd,
            ffi::PROC_PIDFDPSHMINFO,
            buf.as_mut_ptr().cast(),
            size,
        )
    };
    if got != size {
        return None;
    }
    let bytes = buf
        .get(ffi::PSHM_VST_SIZE..ffi::PSHM_VST_SIZE + 8)
        .and_then(|b| <[u8; 8]>::try_from(b).ok())?;
    // `off_t`, signed: a negative size is no object a window made.
    u64::try_from(i64::from_ne_bytes(bytes)).ok()
}

/// See the macOS twin: no reader here, so no descriptor is shared memory the keeper takes.
#[cfg(all(unix, not(target_vendor = "apple")))]
#[must_use]
pub fn fd_is_pshm(_fd: std::os::fd::BorrowedFd<'_>) -> Option<u64> {
    None
}

/// See the macOS twin: no reader here.
///
/// # Errors
///
/// Always `Unsupported`.
#[cfg(all(unix, not(target_vendor = "apple")))]
pub fn pshm_sizes_of(_pid: u32) -> io::Result<Vec<u64>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "a process's shared-memory descriptors are read through libproc (macOS)",
    ))
}

#[path = "holders_paths.rs"]
mod paths;
#[cfg(all(debug_assertions, target_vendor = "apple"))]
pub use paths::a_force_detached_volume_is_no_working_directory;
#[cfg(all(windows, any(test, debug_assertions)))]
pub use paths::scan_path_users_of_within;
pub use paths::{Exe, PathScan, PathUse, exe_of, scan_path_users, scan_path_users_of};

/// Test seams: run a closure between one table's listing and the reads of its descriptors (to
/// make a race certain), read a dead vnode as unknown (for a negative control), and read a
/// process's working directory as a dead vnode (a scratch folder standing in for a volume
/// force-unmounted under it).
#[cfg(all(any(test, debug_assertions), target_vendor = "apple"))]
mod hook {
    use std::cell::Cell;
    #[cfg(test)]
    use std::cell::RefCell;

    use super::Failed;

    #[cfg(test)]
    type Hook = Box<dyn FnOnce(i32, &[(i32, u32)])>;

    thread_local! {
        #[cfg(test)]
        static AFTER_LISTING: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static DEAD_AS_UNKNOWN: Cell<bool> = const { Cell::new(false) };
        #[cfg(test)]
        static CWD_DEAD: Cell<Option<i32>> = const { Cell::new(None) };
    }

    /// Run `f` with this thread's reads of `pid`'s working directory answering `ENOENT`, as
    /// a reclaimed vnode's do once its volume is force-unmounted, without mounting one:
    /// reading inside a mounted disk image can raise macOS's removable-volume dialog.
    #[cfg(test)]
    pub fn reading_cwd_dead<R>(pid: i32, f: impl FnOnce() -> R) -> R {
        CWD_DEAD.with(|d| d.set(Some(pid)));
        let out = f();
        CWD_DEAD.with(|d| d.set(None));
        out
    }

    /// Whether this thread reads `pid`'s working directory as a dead vnode.
    #[cfg(test)]
    pub(super) fn cwd_dead(pid: i32) -> bool {
        CWD_DEAD.with(Cell::get) == Some(pid)
    }

    /// Run `f` with this thread's dead-vnode `ENOENT`s read as unknown failures.
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
    #[cfg(test)]
    pub fn after_next_listing(f: impl FnOnce(i32, &[(i32, u32)]) + 'static) {
        AFTER_LISTING.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    }

    #[cfg(test)]
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

    /// XNU's kernel-private `EREDRIVEOPEN`: a `/dev/ptmx` open that lost the race for a pty
    /// minor to another opener in any process. aterm-pty's `open_ptmx` redrives it for aterm's
    /// pairs; this crate sits below aterm-pty, so its test opener redrives it here.
    const EREDRIVEOPEN: i32 = -6;

    /// A fresh PTY master (a device no other process holds) at the lowest free number at or
    /// above `at`, and its rdev. A lost race ([`EREDRIVEOPEN`]) is opened again; any other
    /// error fails at once.
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

    /// A descriptor closed between the listing and its read (`EBADF`) is not held and does not
    /// make the table unknown. Controls: a second descriptor for the same master above the
    /// closed one is still found, and any other failure still reads as unknown.
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
        assert_eq!(Failed::of(1), Failed::Refused, "EPERM");
        for other in [0, 12, 13, 22] {
            assert_eq!(Failed::of(other), Failed::Other, "errno {other}");
        }
    }

    /// A `sleep` whose stdin is `/dev/null`, once the kernel shows it holding the device, and
    /// that device's rdev. Spawned by `spawnfd`, so it holds no other test thread's pipe.
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
        // Wait for the exec: ask for the child's table until it names the device.
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

    /// Asked about several devices at once (a child's `/dev/null`, our fresh PTY master asked
    /// twice, a device nobody holds), each answer in order is the one a scan for that device
    /// alone gives; `skip` holds for all of them; no device asked answers nothing. Negative
    /// control: each holder is found for its own device only.
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
            // Whether this process holds `/dev/null` at a given moment turns on its sibling
            // tests (a `Stdio::null()` spawn opens it here for a moment): the child answers.
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

    /// libproc's `pshm_fdinfo` is the size and layout the reader assumes: a region's read-only
    /// custody descriptor and a writable twin both read back as shm of the size they were made,
    /// at two sizes (so the offset reads the size, not a constant); a vnode, a socket and a pipe
    /// are no shm.
    #[test]
    fn a_shared_memory_descriptor_is_named_with_its_size_and_nothing_else_is() {
        use std::os::fd::AsFd as _;
        assert_eq!(ffi::PSHM_FDINFO_SIZE, 1192);
        assert_eq!(ffi::PSHM_VST_SIZE, 112);
        for len in [crate::shm::PAGE, 5 * crate::shm::PAGE] {
            // SAFETY: this private custody is queried only for metadata, never reopened
            // or used to modify/resize the object; both local owners drop this iteration.
            let (_region, custody) = unsafe { crate::shm::Region::create(len) }.expect("a region");
            assert_eq!(super::fd_is_pshm(custody.as_fd()), Some(len as u64));
            // SAFETY: this private descriptor is queried only for metadata, never used to
            // write, resize or map; both owners stay in this iteration until they drop.
            let (_twin, writable) =
                unsafe { crate::shm::testing::create_with_writable_custody(len) }.expect("a twin");
            assert_eq!(super::fd_is_pshm(writable.as_fd()), Some(len as u64));
        }
        let file = std::fs::File::open("/dev/null").expect("/dev/null");
        assert_eq!(super::fd_is_pshm(file.as_fd()), None, "a vnode");
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        assert_eq!(super::fd_is_pshm(a.as_fd()), None, "a socket");
        let (r, _w) = std::io::pipe().expect("a pipe");
        assert_eq!(super::fd_is_pshm(r.as_fd()), None, "a pipe");
    }

    /// The kernel's table names a process's shared-memory descriptors with their sizes: a region
    /// held shows (its custody descriptor; the creator's mapping is no descriptor), and once its
    /// descriptor closes it is gone. The size is one no other test in this binary makes, since
    /// they run beside this one in the same process.
    #[test]
    fn a_process_s_shared_memory_descriptors_are_listed_with_their_sizes() {
        let len = 13 * crate::shm::PAGE;
        let mine = || {
            super::pshm_sizes_of(std::process::id())
                .expect("our own table")
                .into_iter()
                .filter(|s| *s == len as u64)
                .count()
        };
        assert_eq!(mine(), 0);
        // SAFETY: this fixture only inspects the descriptor table and drops these local
        // owners; no descriptor or alias modifies, maps or resizes the private object.
        let (region, custody) = unsafe { crate::shm::Region::create(len) }.expect("a region");
        assert_eq!(mine(), 1, "the custody descriptor");
        drop(region);
        assert_eq!(mine(), 1, "the mapping was no descriptor");
        drop(custody);
        assert_eq!(mine(), 0, "closed");
    }
}
