// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Same-UID process descriptor and path holder scans.

use std::io;

/// Result of scanning same-UID processes for open descriptors.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HolderScan {
    /// Processes holding at least one matching descriptor.
    pub holders: Vec<u32>,
    /// Processes whose descriptor table could not be read.
    pub unreadable: Vec<u32>,
    /// Total processes scanned.
    pub scanned: usize,
}

impl HolderScan {
    /// Whether `pid` holds the target.
    #[must_use]
    pub fn holds(&self, pid: u32) -> bool {
        self.holders.contains(&pid)
    }
}

/// Scan live same-UID processes except `skip` for descriptors matching `rdev`.
///
/// # Errors
/// The process listing itself failing; `Unsupported` off macOS.
///
/// Returns an error if the process listing fails.
pub fn scan_rdev_holders(rdev: u64, skip: &[u32]) -> io::Result<HolderScan> {
    Ok(scan_holders_of(&[rdev], skip)?.pop().unwrap_or_default())
}

/// The `st_rdev` of the master of the PTY whose slave is `slave` (a process's controlling
/// terminal, `e_tdev`), for a scan that asks whether a live process still holds that
/// terminal's master. Darwin numbers a PTY's two ends with one minor, and every master's major
/// is `/dev/ptmx`'s (measured on Darwin 27: master 15/4, slave `/dev/ttys004` 16/4, `/dev/ptmx`
/// 15/n). `None` when `/dev/ptmx` cannot be read, and off macOS. A `slave` that is no PTY maps
/// to a number no master or some unrelated master has, so a scan of it proves nothing for
/// that terminal either way.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn pty_master_rdev(slave: u64) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    // Darwin's 32-bit `dev_t`: the major in the top 8 bits, the minor in the low 24.
    // `MetadataExt` widens it with its sign, so only the low 32 bits are read.
    const DEV: u64 = 0xffff_ffff;
    const MINOR: u64 = 0x00ff_ffff;
    let ptmx = std::fs::metadata("/dev/ptmx").ok()?.rdev() & DEV;
    Some((ptmx & !MINOR) | (slave & MINOR))
}

/// See the macOS twin.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn pty_master_rdev(_slave: u64) -> Option<u64> {
    None
}

/// Scan live same-UID processes except `skip` for each device in `rdevs`.
///
/// # Errors
///
/// Returns an error if the process listing fails.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn scan_holders_of(rdevs: &[u64], skip: &[u32]) -> io::Result<Vec<HolderScan>> {
    let wanted: Vec<u128> = rdevs.iter().map(|r| u128::from(*r)).collect();
    scan(&wanted, Lookup::BY_RDEV, skip)
}

/// Scan live same-UID processes except `skip` for descriptors open on `path`.
///
/// # Errors
/// `path` cannot be read (`stat`), or the process listing fails; `Unsupported` off macOS.
///
/// Returns an error if `path` cannot be stat'd or the process listing fails.
#[cfg(target_vendor = "apple")]
pub fn scan_file_holders(path: &std::path::Path, skip: &[u32]) -> io::Result<HolderScan> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path)?;
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

/// Scan live same-UID processes except `skip` for descriptors open on `path`.
///
/// # Errors
/// Always `Unsupported`.
///
/// Returns `Unsupported` on platforms without a holder scan.
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

/// Scan live same-UID processes except `skip` for each device in `rdevs`.
///
/// # Errors
/// Always `Unsupported`.
///
/// Returns `Unsupported` on platforms without a holder scan.
#[cfg(not(target_vendor = "apple"))]
pub fn scan_holders_of(_rdevs: &[u64], _skip: &[u32]) -> io::Result<Vec<HolderScan>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the holder scan reads libproc (macOS)",
    ))
}

/// Whether any standard descriptor (`0..=2`) of `pid` sits on a revoked vnode.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn stdio_on_dead_vnode(pid: u32) -> Option<bool> {
    let pid = i32::try_from(pid).ok()?;
    for (fd, kind) in list_fds(pid).ok()? {
        if !(0..=2).contains(&fd) || kind != ffi::PROX_FDTYPE_VNODE {
            continue;
        }
        match vnode_of(pid, fd, Lookup::BY_RDEV) {
            Ok(_) | Err(Failed::Closed) => {}
            Err(Failed::Dead) => return Some(true),
            Err(Failed::Exited | Failed::Refused | Failed::Other) => return None,
        }
    }
    Some(false)
}

/// Whether any standard descriptor (`0..=2`) of `pid` sits on a revoked vnode.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn stdio_on_dead_vnode(_pid: u32) -> Option<bool> {
    None
}

/// Whether process `pid` holds a descriptor for `rdev`.
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
    Dead,
    /// `EPERM`: the kernel will not tell this process.
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
    };
    /// A file by its device and inode, read without the path
    /// (`PROC_PIDFDVNODEINFO`), which the match does not need.
    const BY_FILE: Self = Self {
        flavor: ffi::PROC_PIDFDVNODEINFO,
        size: ffi::VNODE_FD_SIZE,
        key: Vnode::file_key,
    };
}

/// What `pid`'s descriptor table says of each key in `wanted`, in order, each vnode
/// descriptor read as `lookup` reads it, from one listing and one read per vnode
/// descriptor.
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
    read_listed_table(wanted, lookup, fds, |fd| vnode_of(pid, fd, lookup))
}

/// Fold a listed table's vnode reads.
#[cfg(target_vendor = "apple")]
fn read_listed_table(
    wanted: &[u128],
    lookup: Lookup,
    fds: Vec<(i32, u32)>,
    mut read: impl FnMut(i32) -> Result<Vnode, Failed>,
) -> Vec<TableRead> {
    let every = |read| vec![read; wanted.len()];
    let mut wanted: Vec<(u128, usize)> = wanted.iter().copied().zip(0..).collect();
    wanted.sort_unstable();
    let mut held = vec![false; wanted.len()];
    let mut found = 0;
    let mut unreadable = false;
    for (fd, kind) in fds {
        if kind != ffi::PROX_FDTYPE_VNODE {
            continue;
        }
        match read(fd) {
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
            Err(Failed::Exited) => return every(TableRead::Exited),
            Err(Failed::Closed | Failed::Dead) => {}
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

/// Whether process `pid` holds a descriptor for `rdev`.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn process_holds_rdev(_pid: u32, _rdev: u64) -> Option<bool> {
    None
}

/// libproc constants and layout offsets verified against the macOS SDK.
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

// `process::kin` lists a process group through this `proc_listpids` too.
#[cfg(target_vendor = "apple")]
pub(crate) mod ffi {
    pub const PROC_UID_ONLY: u32 = 4;
    pub const PROC_PIDLISTFDS: i32 = 1;
    pub const PROC_PIDFDVNODEPATHINFO: i32 = 2;
    /// `PROC_PIDFDVNODEINFO` flavor.
    pub const PROC_PIDFDVNODEINFO: i32 = 1;
    /// `sizeof(struct vnode_fdinfo)`.
    pub const VNODE_FD_SIZE: usize = 176;
    pub const PROX_FDTYPE_VNODE: u32 = 1;
    /// `PROX_FDTYPE_PSHM` descriptor type.
    pub const PROX_FDTYPE_PSHM: u32 = 3;
    /// `sizeof(struct proc_fdinfo)`.
    pub const FDINFO_SIZE: usize = 8;
    /// `sizeof(struct vnode_fdinfowithpath)`.
    pub const VNODE_PATH_SIZE: usize = 1200;
    /// `offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_dev)`.
    pub const VST_DEV: usize = 24;
    /// `offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_ino)`.
    pub const VST_INO: usize = 32;
    /// `offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_rdev)`.
    pub const VST_RDEV: usize = 140;
    /// `offsetof(struct vnode_fdinfowithpath, pvip.vip_path)`.
    pub const VNODE_PATH: usize = 176;
    /// `MAXPATHLEN`.
    pub const MAXPATHLEN: usize = 1024;
    /// `PROC_PIDVNODEPATHINFO` flavor.
    pub const PROC_PIDVNODEPATHINFO: i32 = 9;
    /// `sizeof(struct proc_vnodepathinfo)`.
    pub const VNODEPATHINFO_SIZE: usize = 2352;
    /// `offsetof(struct proc_vnodepathinfo, pvi_cdir.vip_path)`.
    pub const CDIR_PATH: usize = 152;
    /// `PROC_PIDPATHINFO_MAXSIZE`.
    pub const PIDPATH_MAX: usize = 4096;
    /// `PROC_PIDFDPSHMINFO` flavor.
    pub const PROC_PIDFDPSHMINFO: i32 = 5;
    /// `sizeof(struct pshm_fdinfo)`.
    pub const PSHM_FDINFO_SIZE: usize = 1192;
    /// `offsetof(struct pshm_fdinfo, pshminfo.pshm_stat.vst_size)`.
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
        /// Darwin's thread-local `errno` pointer.
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

/// List every live process PID owned by this UID.
///
/// # Errors
///
/// Returns an error if the process listing fails.
#[cfg(target_vendor = "apple")]
pub fn uid_pids() -> io::Result<Vec<u32>> {
    // SAFETY: `geteuid` is a side-effect-free getter.
    let uid = unsafe { ffi::geteuid() };
    let mut room = 1024usize;
    loop {
        let mut pids = vec![0i32; room];
        let bytes = i32::try_from(room * 4).map_err(|_| io::Error::other("pid list too large"))?;
        // SAFETY: `pids` has `bytes` writable bytes of `i32` slots.
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

/// List every live process PID owned by this UID.
///
/// # Errors
///
/// Returns an error if `/proc` cannot be read.
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

/// List every live process PID owned by this user SID.
///
/// # Errors
///
/// Returns an error if the process token or WTS enumeration fails.
#[cfg(windows)]
pub fn uid_pids() -> io::Result<Vec<u32>> {
    win_pids::same_user_pids()
}

/// This process's token-user SID and the session manager's process listing: the
/// Windows [`uid_pids`] listing.
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
        // SAFETY: `GetCurrentProcess` is a pseudo-handle and `token` is a live out-pointer.
        if unsafe { ffi::OpenProcessToken(ffi::GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut len: u32 = 0;
        // SAFETY: null buffer queries the required byte length in `len`.
        unsafe { ffi::GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            // SAFETY: closes the token opened above.
            let _ = unsafe { ffi::CloseHandle(token) };
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` has `len` writable bytes for `TOKEN_USER`.
        let ok = unsafe {
            ffi::GetTokenInformation(token, TOKEN_USER, buf.as_mut_ptr().cast(), len, &mut len)
        };
        // SAFETY: closes the token opened above.
        let _ = unsafe { ffi::CloseHandle(token) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
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
        // SAFETY: level and out-params are live locals.
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
        // SAFETY: `info` points to `count` valid `WtsProcessInfoExW` entries.
        let entries = unsafe { std::slice::from_raw_parts(info, count as usize) };
        for e in entries {
            // SAFETY: `e.pUserSid` and `me` are valid SIDs for the duration of the call.
            let same = !e.pUserSid.is_null()
                && unsafe { ffi::IsValidSid(e.pUserSid) } != 0
                && unsafe { ffi::EqualSid(own, e.pUserSid) } != 0;
            if same && e.ProcessId != 0 {
                pids.push(e.ProcessId);
            }
        }
        // SAFETY: frees the buffer allocated by `WTSEnumerateProcessesExW`.
        let _ = unsafe { ffi::WTSFreeMemoryExW(WTS_PROCESS_INFO_LEVEL1, info.cast(), count) };
        Ok(pids)
    }
}

/// List every live process PID owned by this UID.
///
/// # Errors
///
/// Returns `Unsupported` on platforms without a reader.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
pub fn uid_pids() -> io::Result<Vec<u32>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the process listing reads libproc (macOS) or /proc (Linux)",
    ))
}

/// `pid`'s descriptors, each `(fd, type)`.
#[cfg(target_vendor = "apple")]
fn list_fds(pid: i32) -> Result<Vec<(i32, u32)>, Failed> {
    clear_errno();
    // SAFETY: null buffer queries the required byte size.
    let need = unsafe { ffi::proc_pidinfo(pid, ffi::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return Err(Failed::last());
    }
    let slots = usize::try_from(need).map_err(|_| Failed::Other)? / ffi::FDINFO_SIZE + 32;
    let mut buf = vec![0u8; slots * ffi::FDINFO_SIZE];
    let size = i32::try_from(buf.len()).map_err(|_| Failed::Other)?;
    clear_errno();
    // SAFETY: `buf` has `size` writable bytes.
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
    let mut buf = [0u8; ffi::VNODE_PATH_SIZE];
    let buf = buf.get_mut(..lookup.size).ok_or(Failed::Other)?;
    let size = i32::try_from(buf.len()).map_err(|_| Failed::Other)?;
    clear_errno();
    // SAFETY: `buf` matches the structure size required by `lookup.flavor`.
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

/// Size of the POSIX shared-memory object named by `fd`, or `None` if not PSHM.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn fd_is_pshm(fd: std::os::fd::BorrowedFd<'_>) -> Option<u64> {
    use std::os::fd::AsRawFd as _;
    pshm_size(i32::try_from(std::process::id()).ok()?, fd.as_raw_fd())
}

/// Sizes of the POSIX shared-memory objects held by `pid`.
///
/// # Errors
///
/// Returns an error if `pid`'s descriptor table cannot be read.
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
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn pshm_size(pid: i32, fd: i32) -> Option<u64> {
    let mut buf = [0u8; ffi::PSHM_FDINFO_SIZE];
    let size = i32::try_from(buf.len()).ok()?;
    // SAFETY: `buf` matches the structure size for `PROC_PIDFDPSHMINFO`.
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
    u64::try_from(i64::from_ne_bytes(bytes)).ok()
}

/// Size of the POSIX shared-memory object named by `fd`, or `None`.
#[cfg(all(unix, not(target_vendor = "apple")))]
#[must_use]
pub fn fd_is_pshm(_fd: std::os::fd::BorrowedFd<'_>) -> Option<u64> {
    None
}

/// Sizes of the POSIX shared-memory objects held by `pid`.
///
/// # Errors
///
/// Returns `Unsupported` on platforms without a PSHM reader.
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
pub use paths::{
    Exe, PathScan, PathUse, exe_of, open_files, scan_path_users, scan_path_users_of,
    write_open_files,
};

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

    /// Run `f` with reads of `pid`'s working directory returning `ENOENT`.
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

    /// Run `f` with dead-vnode `ENOENT` results treated as unknown failures.
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

    /// Run `f` once after the next descriptor listing on this thread.
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
        // SAFETY: `F_DUPFD_CLOEXEC` duplicates a valid descriptor we own.
        let high = unsafe { fcntl(fd.as_raw_fd(), F_DUPFD_CLOEXEC, at) };
        assert!(
            high >= at,
            "F_DUPFD_CLOEXEC at {at}: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: wraps the newly duplicated descriptor.
        unsafe { std::os::fd::OwnedFd::from_raw_fd(high) }
    }

    /// XNU's kernel-private `EREDRIVEOPEN`: a `/dev/ptmx` open that lost the race for a pty
    /// minor to another opener in any process.
    const EREDRIVEOPEN: i32 = -6;

    /// A fresh PTY master (a device no other process holds) at the lowest free number at or
    /// above `at`, and its rdev.
    pub(super) fn a_master_at(at: i32) -> (std::os::fd::OwnedFd, u64) {
        use std::os::fd::FromRawFd as _;
        const O_RDWR: i32 = 2;
        const O_NOCTTY: i32 = 0x20000;
        const O_CLOEXEC: i32 = 0x0100_0000;
        let started = Instant::now();
        let raw = loop {
            // SAFETY: `posix_openpt` returns a fresh descriptor owned by this test.
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
            assert!(
                started.elapsed() < HANG,
                "posix_openpt lost every race for {HANG:?}"
            );
        };
        // SAFETY: wraps the fresh descriptor owned by this test.
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
    /// make the table unknown.
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

        use super::Failed;
        assert_eq!(Failed::of(3), Failed::Exited, "ESRCH");
        assert_eq!(Failed::of(9), Failed::Closed, "EBADF");
        assert_eq!(Failed::of(2), Failed::Dead, "ENOENT");
        assert_eq!(Failed::of(1), Failed::Refused, "EPERM");
        for other in [0, 12, 13, 22] {
            assert_eq!(Failed::of(other), Failed::Other, "errno {other}");
        }

        let vnode = super::Vnode {
            dev: 7,
            ino: 9,
            rdev: 11,
        };
        for lookup in [super::Lookup::BY_FILE, super::Lookup::BY_RDEV] {
            let wanted = (lookup.key)(&vnode);
            let fds = || vec![(3, ffi::PROX_FDTYPE_VNODE)];
            assert_eq!(
                super::read_listed_table(&[wanted], lookup, fds(), |_| Err(Failed::Refused)),
                [super::TableRead::Unreadable],
                "EPERM is unknown for every unconfirmed key"
            );
            assert_eq!(
                super::read_listed_table(&[wanted], lookup, fds(), |_| Ok(vnode)),
                [super::TableRead::Holds],
                "a positive match remains a positive match"
            );
            assert_eq!(
                super::read_listed_table(
                    &[wanted, wanted + 1],
                    lookup,
                    vec![(3, ffi::PROX_FDTYPE_VNODE), (4, ffi::PROX_FDTYPE_VNODE)],
                    |fd| if fd == 3 {
                        Err(Failed::Refused)
                    } else {
                        Ok(vnode)
                    },
                ),
                [super::TableRead::Holds, super::TableRead::Unreadable],
                "a refusal obscures only keys not positively found"
            );
        }
    }

    /// A `sleep` whose stdin is `/dev/null`, once the kernel shows it holding the device, and
    /// that device's rdev.
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
        // SAFETY: sends `SIGKILL` to the child process spawned by this test.
        unsafe { kill(pid as i32, 9) };
        let started = Instant::now();
        while watch.exit_status().is_none() {
            assert!(started.elapsed() < HANG, "the child never exited");
            std::thread::yield_now();
        }
    }

    /// An exited process holds nothing: a killed, unreaped child (a zombie in
    /// the listing) is a non-holder, never unreadable.
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
    /// alone gives; `skip` holds for all of them; no device asked answers nothing.
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
            // SAFETY: private custody descriptor is inspected only for metadata.
            let (_region, custody) = unsafe { crate::shm::Region::create(len) }.expect("a region");
            assert_eq!(super::fd_is_pshm(custody.as_fd()), Some(len as u64));
            // SAFETY: private writable descriptor is inspected only for metadata.
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
    /// descriptor closes it is gone.
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
        // SAFETY: fixture only inspects the descriptor table and drops local handles.
        let (region, custody) = unsafe { crate::shm::Region::create(len) }.expect("a region");
        assert_eq!(mine(), 1, "the custody descriptor");
        drop(region);
        assert_eq!(mine(), 1, "the mapping was no descriptor");
        drop(custody);
        assert_eq!(mine(), 0, "closed");
    }
}
