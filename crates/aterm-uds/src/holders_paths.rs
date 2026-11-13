// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Executable, mapped-file, descriptor and working-directory holder inspection.
use std::io;
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
use std::path::Path;
use std::path::PathBuf;

#[cfg(any(target_vendor = "apple", target_os = "linux", all(test, unix)))]
#[path = "holders_mappings.rs"]
mod mappings;

/// One process using a path under one of the directories scanned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathUse {
    pub pid: u32,
    pub how: &'static str,
    pub path: PathBuf,
}

/// What one scan found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PathScan {
    pub users: Vec<PathUse>,
    pub unreadable: Vec<(u32, Option<PathBuf>)>,
    pub scanned: usize,
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn placed(path: &Path, dirs: &[PathBuf]) -> Option<PathBuf> {
    if dirs.iter().any(|d| path.starts_with(d)) {
        return Some(path.to_path_buf());
    }
    let data = Path::new("/").join(path.strip_prefix("/System/Volumes/Data").ok()?);
    dirs.iter().any(|d| data.starts_with(d)).then_some(data)
}

#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn scan_path_users(dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    scan_path_users_of(&super::uid_pids()?, dirs, skip)
}

#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn scan_path_users_of(pids: &[u32], dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    let mut scan = PathScan::default();
    for &pid in pids {
        if skip.contains(&pid) {
            continue;
        }
        crate::progress::tick();
        scan.scanned += 1;
        match mac::uses(pid, dirs) {
            Ok(Some(found)) if !unexeced_fork(pid, skip) => scan.users.push(found),
            Ok(Some(_)) => {}
            Ok(None) | Err((super::Failed::Exited, _)) => {}
            // A process of this user's that became a set-user-ID program (`/bin/ps`, `top`)
            // since the listing is refused (`EPERM`) and outside the scan, as it is once listed
            // so ([`super::runs_as_another_user`]).
            Err(_) if super::runs_as_another_user(pid) => {}
            Err((
                super::Failed::Closed
                | super::Failed::Dead
                | super::Failed::Refused
                | super::Failed::Other,
                exe,
            )) => {
                scan.unreadable.push((pid, exe));
            }
        }
    }
    Ok(scan)
}

/// The path of every file `pid` holds open through a descriptor, in descriptor order, as the
/// kernel names it: what `lsof -p <pid>` names for its descriptors, read in this process. A
/// descriptor closed or revoked mid-read names nothing, and so does one whose metadata the
/// kernel keeps from this process once that refusal is shown to be the file's and not the
/// process's (`refusal_is_the_vnodes`).
///
/// # Errors
///
/// `NotFound` when `pid` has exited; `PermissionDenied` when the kernel will not list the
/// table or describe a descriptor for a reason not shown to be the file's; `Other` for any
/// other failure; `Unsupported` off macOS and Linux.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn open_files(pid: u32) -> io::Result<Vec<PathBuf>> {
    mac::open_files(pid).map_err(|failed| {
        let kind = match failed {
            super::Failed::Exited => io::ErrorKind::NotFound,
            super::Failed::Refused => io::ErrorKind::PermissionDenied,
            super::Failed::Closed | super::Failed::Dead | super::Failed::Other => {
                io::ErrorKind::Other
            }
        };
        io::Error::new(kind, format!("the descriptors of {pid}: {failed:?}"))
    })
}

/// See the macOS twin. Linux: each `/proc/<pid>/fd/<n>` link that names a path (a pipe,
/// socket or anonymous inode names none), as `lsof` reads them, in descriptor order, read
/// from the links alone (`holders_procfs.rs`).
///
/// # Errors
///
/// `NotFound` when `pid` has exited or is a zombie; `PermissionDenied` when its table is not
/// this process's to read (another uid's, or a non-dumpable process's); the read's error
/// otherwise.
#[cfg(target_os = "linux")]
pub fn open_files(pid: u32) -> io::Result<Vec<PathBuf>> {
    super::procfs::open_files_in(Path::new(super::procfs::PROC), pid)
}

/// See the macOS twin.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub fn open_files(_pid: u32) -> io::Result<Vec<PathBuf>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "a process's descriptors are read in process through libproc (macOS) or /proc (Linux)",
    ))
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn unexeced_fork(pid: u32, parents: &[u32]) -> bool {
    let Some(ppid) = parent_of(pid) else {
        return false;
    };
    parents.contains(&ppid)
        && match (executable(pid), executable(ppid), own_program()) {
            (Some(child), Some(parent), Some(own)) => child == parent && parent == own,
            _ => false,
        }
}

#[cfg(target_vendor = "apple")]
fn own_program() -> Option<PathBuf> {
    executable(std::process::id())
}

#[cfg(target_os = "linux")]
fn own_program() -> Option<PathBuf> {
    let kernel = executable(std::process::id())?;
    let untranslated =
        std::fs::read_link("/proc/self/exe").ok()? == kernel && own_image_machine()? == ELF_MACHINE;
    untranslated.then_some(kernel)
}

#[cfg(target_os = "linux")]
fn own_image_machine() -> Option<u16> {
    use std::io::Read as _;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(format!("/proc/{}", std::process::id()))
        .ok()?;
    // SAFETY: `dir` is open for the call and the name is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c"exe".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return None;
    }
    // SAFETY: `openat` just returned `fd`, which nothing else owns.
    let mut image = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    let mut head = [0u8; 20];
    image.read_exact(&mut head).ok()?;
    let [0x7f, b'E', b'L', b'F', _, data, .., m0, m1] = head else {
        return None;
    };
    match data {
        1 => Some(u16::from_le_bytes([m0, m1])),
        2 => Some(u16::from_be_bytes([m0, m1])),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
const ELF_MACHINE: u16 = if cfg!(target_arch = "x86_64") {
    62
} else if cfg!(target_arch = "aarch64") {
    183
} else {
    0
};

#[cfg(target_vendor = "apple")]
fn parent_of(pid: u32) -> Option<u32> {
    const PROC_PIDT_SHORTBSDINFO: i32 = 13;
    const SIZE: usize = 64;
    let n = i32::try_from(pid).ok()?;
    let mut buf = [0u8; SIZE];
    // SAFETY: `buf` is exactly the structure this flavor fills.
    let got = unsafe {
        super::ffi::proc_pidinfo(
            n,
            PROC_PIDT_SHORTBSDINFO,
            0,
            buf.as_mut_ptr().cast(),
            i32::try_from(SIZE).ok()?,
        )
    };
    if usize::try_from(got).ok() != Some(SIZE) {
        return None;
    }
    let b = buf.get(4..8)?;
    Some(u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
}

/// What the kernel says of a process's executable ([`exe_of`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exe {
    At(PathBuf),
    Unnamed,
    Exited,
}

/// The executable of `pid`, as the kernel holds it, told apart from a process that has exited.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn exe_of(pid: u32) -> Exe {
    match i32::try_from(pid).map(mac::exe) {
        Ok(Ok(Some(path))) => Exe::At(path),
        Ok(Err(super::Failed::Exited)) => Exe::Exited,
        _ => Exe::Unnamed,
    }
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn executable(pid: u32) -> Option<PathBuf> {
    match exe_of(pid) {
        Exe::At(path) => Some(path),
        Exe::Unnamed | Exe::Exited => None,
    }
}

#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<u32> {
    let stat = crate::process::read_proc_text(Path::new(&format!("/proc/{pid}/stat"))).ok()?;
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    rest.split_whitespace()
        .nth(1)
        .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()
}

/// See the macOS twin.
#[cfg(target_os = "linux")]
#[must_use]
pub fn exe_of(pid: u32) -> Exe {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let gone = |e: &io::Error| matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ESRCH));
    let dir = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(format!("/proc/{pid}"))
    {
        Ok(dir) => dir,
        Err(e) if gone(&e) => return Exe::Exited,
        Err(_) => return Exe::Unnamed,
    };
    let mut buf = [0u8; 4096];
    // SAFETY: `dir` is open, `name` is NUL-terminated, and `readlinkat` writes at most `buf.len()` bytes.
    let n = unsafe {
        libc::readlinkat(
            dir.as_raw_fd(),
            c"exe".as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    let Ok(len) = usize::try_from(n) else {
        return if gone(&io::Error::last_os_error()) {
            Exe::Exited
        } else {
            Exe::Unnamed
        };
    };
    match buf.get(..len) {
        Some(path) if len < buf.len() => {
            Exe::At(PathBuf::from(std::ffi::OsString::from_vec(path.to_vec())))
        }
        _ => Exe::Unnamed,
    }
}

/// See the macOS twin: no reader here, so none is named.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
#[must_use]
pub fn exe_of(_pid: u32) -> Exe {
    Exe::Unnamed
}

/// The working directory of `pid`, as the kernel answers it: none for a process gone, another
/// user's, or a directory with no path.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    mac::cwd(i32::try_from(pid).ok()?).ok().flatten()
}

/// See the macOS twin: `/proc/<pid>/cwd`'s target.
#[cfg(target_os = "linux")]
#[must_use]
pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .filter(|p| p.is_absolute())
}

/// See the macOS twin: no reader here, so none is named.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
#[must_use]
pub fn cwd_of(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(target_vendor = "apple")]
mod mac {
    use super::super::{Failed, clear_errno, ffi, list_fds};
    use super::{PathBuf, PathUse, placed};
    use std::os::unix::ffi::OsStrExt as _;

    fn path_at(buf: &[u8], at: usize, len: usize) -> Option<PathBuf> {
        let bytes = buf.get(at..at.checked_add(len)?)?;
        let n = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        let p = std::path::Path::new(std::ffi::OsStr::from_bytes(bytes.get(..n)?));
        p.is_absolute().then(|| p.to_path_buf())
    }

    /// The working directory of `pid`, as the kernel answers it.
    pub(super) fn cwd(pid: i32) -> Result<Option<PathBuf>, Failed> {
        #[cfg(test)]
        if super::super::hook::cwd_dead(pid) {
            return Err(super::super::hook::as_read(Failed::Dead));
        }
        let mut buf = [0u8; ffi::VNODEPATHINFO_SIZE];
        let size = i32::try_from(buf.len()).map_err(|_| Failed::Other)?;
        clear_errno();
        // SAFETY: `buf` is exactly the structure this flavor fills.
        let got = unsafe {
            ffi::proc_pidinfo(
                pid,
                ffi::PROC_PIDVNODEPATHINFO,
                0,
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
        Ok(path_at(&buf, ffi::CDIR_PATH, ffi::MAXPATHLEN))
    }

    /// The executable of `pid`.
    pub(super) fn exe(pid: i32) -> Result<Option<PathBuf>, Failed> {
        let mut buf = [0u8; ffi::PIDPATH_MAX];
        let size = u32::try_from(buf.len()).map_err(|_| Failed::Other)?;
        clear_errno();
        // SAFETY: `buf` is `size` writable bytes; the call writes at most that.
        let got = unsafe { ffi::proc_pidpath(pid, buf.as_mut_ptr().cast(), size) };
        if got <= 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if errno == Failed::ENOENT {
                return Ok(None);
            }
            return Err(Failed::of(errno));
        }
        let n = usize::try_from(got).map_err(|_| Failed::Other)?;
        Ok(path_at(&buf, 0, n.min(buf.len())))
    }

    fn fd_path(pid: i32, fd: i32) -> Result<Option<PathBuf>, Failed> {
        #[cfg(test)]
        if super::super::hook::refuses(pid, fd) {
            return Err(Failed::Refused);
        }
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
        Ok(path_at(&buf, ffi::VNODE_PATH, ffi::MAXPATHLEN))
    }

    const FI_OPENFLAGS: usize = 0;
    const FWRITE: u32 = 0x0002;
    const VST_MODE: usize = 28;
    const S_IFMT: u16 = 0o170_000;
    const S_IFREG: u16 = 0o100_000;

    /// The regular files `pid` holds open for writing, as the kernel names them.
    pub(super) fn writes(pid: u32) -> Result<Vec<PathBuf>, Failed> {
        let n = i32::try_from(pid).map_err(|_| Failed::Other)?;
        let fds = match list_fds(n) {
            Ok(fds) => fds,
            Err(Failed::Exited) => return Ok(Vec::new()),
            Err(other) => return Err(other),
        };
        let mut out = Vec::new();
        for (fd, kind) in fds {
            if kind != ffi::PROX_FDTYPE_VNODE {
                continue;
            }
            let mut buf = [0u8; ffi::VNODE_PATH_SIZE];
            let size = i32::try_from(buf.len()).map_err(|_| Failed::Other)?;
            clear_errno();
            // SAFETY: `buf` is exactly the structure this flavor fills.
            let got = unsafe {
                ffi::proc_pidfdinfo(
                    n,
                    fd,
                    ffi::PROC_PIDFDVNODEPATHINFO,
                    buf.as_mut_ptr().cast(),
                    size,
                )
            };
            if got <= 0 {
                match Failed::last() {
                    Failed::Exited => return Ok(Vec::new()),
                    Failed::Closed | Failed::Dead | Failed::Refused => continue,
                    Failed::Other => return Err(Failed::Other),
                }
            }
            if got != size {
                return Err(Failed::Other);
            }
            let flags = buf
                .get(FI_OPENFLAGS..FI_OPENFLAGS + 4)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map(u32::from_ne_bytes)
                .ok_or(Failed::Other)?;
            let mode = buf
                .get(VST_MODE..VST_MODE + 2)
                .and_then(|b| <[u8; 2]>::try_from(b).ok())
                .map(u16::from_ne_bytes)
                .ok_or(Failed::Other)?;
            if flags & FWRITE == 0 || mode & S_IFMT != S_IFREG {
                continue;
            }
            if let Some(path) = path_at(&buf, ffi::VNODE_PATH, ffi::MAXPATHLEN) {
                out.push(path);
            }
        }
        Ok(out)
    }

    /// [`super::open_files`]: a descriptor refused for its vnode (`refusal_is_the_vnodes`) names nothing.
    pub(super) fn open_files(pid: u32) -> Result<Vec<PathBuf>, Failed> {
        let n = i32::try_from(pid).map_err(|_| Failed::Other)?;
        let mut files = Vec::new();
        let mut refused = Vec::new();
        let mut described = false;
        for (fd, kind) in list_fds(n)? {
            if kind != ffi::PROX_FDTYPE_VNODE {
                continue;
            }
            match fd_path(n, fd) {
                Ok(path) => {
                    described = true;
                    files.extend(path);
                }
                Err(Failed::Closed | Failed::Dead) => {}
                Err(Failed::Refused) => refused.push(fd),
                Err(failed) => return Err(failed),
            }
        }
        // Each is read again without its path: refused so too, what the kernel keeps from
        // this process is its vnode's metadata, not only its path.
        let lookup = super::super::Lookup::BY_FILE;
        if refused.is_empty()
            || (described && super::super::refusal_is_the_vnodes(n, &refused, lookup))
        {
            Ok(files)
        } else {
            Err(Failed::Refused)
        }
    }

    /// The first use `pid` makes of a path under `dirs`.
    pub(super) fn uses(
        pid: u32,
        dirs: &[PathBuf],
    ) -> Result<Option<PathUse>, (Failed, Option<PathBuf>)> {
        let n = i32::try_from(pid).map_err(|_| (Failed::Other, None))?;
        let hit = |how: &'static str, path: Option<PathBuf>| {
            path.and_then(|p| placed(&p, dirs))
                .map(|path| PathUse { pid, how, path })
        };
        let exe = exe(n).map_err(|f| (f, None))?;
        let fail = |f: Failed| (f, exe.clone());
        if let Some(found) = hit("exe", exe.clone()) {
            return Ok(Some(found));
        }
        let cwd = match cwd(n) {
            Ok(path) => path,
            Err(Failed::Dead) => None,
            Err(other) => return Err(fail(other)),
        };
        if let Some(found) = hit("cwd", cwd) {
            return Ok(Some(found));
        }
        // A remover may ignore its own lock descriptors; they must not hide a mapping.
        if let Some(found) = super::mappings::mac_uses(pid, dirs).map_err(fail)? {
            return Ok(Some(found));
        }
        let mut unreadable = false;
        let fds = list_fds(n).map_err(fail)?;
        #[cfg(test)]
        super::super::hook::after_listing(n, &fds);
        for (fd, kind) in fds {
            if kind != ffi::PROX_FDTYPE_VNODE {
                continue;
            }
            match fd_path(n, fd) {
                Ok(p) => {
                    if let Some(found) = hit("fd", p) {
                        return Ok(Some(found));
                    }
                }
                Err(Failed::Exited) => return Err(fail(Failed::Exited)),
                Err(Failed::Closed | Failed::Dead) => {}
                Err(Failed::Refused | Failed::Other) => unreadable = true,
            }
        }
        if unreadable {
            Err(fail(Failed::Other))
        } else {
            Ok(None)
        }
    }
}

/// See the macOS twin.
#[cfg(target_os = "linux")]
pub fn scan_path_users(dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    scan_path_users_of(&super::uid_pids()?, dirs, skip)
}

/// See the macOS twin.
#[cfg(target_os = "linux")]
pub fn scan_path_users_of(pids: &[u32], dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    let mut scan = PathScan::default();
    for &pid in pids {
        if skip.contains(&pid) {
            continue;
        }
        crate::progress::tick();
        let proc_dir = PathBuf::from(format!("/proc/{pid}"));
        if std::fs::symlink_metadata(&proc_dir).is_err() {
            // Distinct branch.
            continue; // exited
        }
        scan.scanned += 1;
        match linux_uses(&proc_dir, pid, dirs) {
            Ok(Some(found)) if !unexeced_fork(pid, skip) => scan.users.push(found),
            Ok(Some(_)) => {}
            Ok(None) => {}
            // A process of this user's that became a set-user-ID program (`sudo`, `su`) since
            // the listing is refused (`EACCES`: the exec made it non-dumpable, its links root's)
            // and outside the scan, as it is once listed so ([`super::runs_as_another_user`]).
            Err(()) if super::runs_as_another_user(pid) => {}
            Err(()) => scan
                .unreadable
                .push((pid, std::fs::read_link(proc_dir.join("exe")).ok())),
        }
    }
    Ok(scan)
}

#[cfg(target_os = "linux")]
fn linux_uses(proc_dir: &Path, pid: u32, dirs: &[PathBuf]) -> Result<Option<PathUse>, ()> {
    let gone = |e: &io::Error| e.kind() == io::ErrorKind::NotFound;
    for how in ["cwd", "exe"] {
        match std::fs::read_link(proc_dir.join(how)) {
            Ok(p) => {
                if let Some(path) = placed(&p, dirs) {
                    return Ok(Some(PathUse { pid, how, path }));
                }
            }
            Err(e) if gone(&e) => {
                // A process with no executable (a kernel thread) has none to
                // read; one that exited has no directory left either.
                if std::fs::symlink_metadata(proc_dir).is_err() {
                    return Ok(None);
                }
            }
            Err(_) => return Err(()),
        }
    }
    // The cache remover's own lock descriptors may be discounted by its consumer.
    if let Some(found) = mappings::linux_uses(proc_dir, pid, dirs)? {
        return Ok(Some(found));
    }
    let rd = match std::fs::read_dir(proc_dir.join("fd")) {
        Ok(rd) => rd,
        Err(e) if gone(&e) => return Ok(None),
        Err(_) => return Err(()),
    };
    for fd in rd.flatten() {
        match std::fs::read_link(fd.path()) {
            Ok(p) => {
                if let Some(path) = placed(&p, dirs) {
                    return Ok(Some(PathUse {
                        pid,
                        how: "fd",
                        path,
                    }));
                }
            }
            Err(e) if gone(&e) => {}
            Err(_) => return Err(()),
        }
    }
    Ok(None)
}

/// See the macOS twin.
#[cfg(windows)]
pub fn scan_path_users(dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    scan_path_users_of(&super::uid_pids()?, dirs, skip)
}

/// See the macOS twin.
#[cfg(windows)]
pub fn scan_path_users_of(pids: &[u32], dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    Ok(scan_naming(pids, dirs, skip, win::Namer::default()))
}

/// Scan processes holding open paths under `roots` with a bounded per-query deadline.
#[cfg(all(windows, any(test, debug_assertions)))]
pub fn scan_path_users_of_within(
    pids: &[u32],
    dirs: &[PathBuf],
    skip: &[u32],
    deadline: std::time::Duration,
) -> io::Result<PathScan> {
    Ok(scan_naming(pids, dirs, skip, win::Namer::within(deadline)))
}

#[cfg(windows)]
fn scan_naming(pids: &[u32], dirs: &[PathBuf], skip: &[u32], mut namer: win::Namer) -> PathScan {
    let table = aterm_sysprobe::volumes::table();
    settled(pids, skip, |pids| {
        let handles = win::file_handles();
        pids.iter()
            .map(|&pid| {
                crate::progress::tick();
                let pid_handles = handles
                    .as_ref()
                    .map(|h| h.get(&pid).map_or(&[][..], Vec::as_slice));
                (pid, win::uses(pid, dirs, &table, pid_handles, &mut namer))
            })
            .collect()
    })
}

#[cfg(windows)]
fn settled(
    pids: &[u32],
    skip: &[u32],
    mut pass: impl FnMut(&[u32]) -> Vec<(u32, win::Read)>,
) -> PathScan {
    let first: Vec<u32> = pids.iter().copied().filter(|p| !skip.contains(p)).collect();
    let mut scan = PathScan {
        scanned: first.len(),
        ..PathScan::default()
    };
    let mut again: Vec<(u32, Option<PathBuf>)> = Vec::new();
    for (pid, read) in pass(&first) {
        match read {
            win::Read::User(found) => scan.users.push(found),
            win::Read::None | win::Read::Exited => {}
            win::Read::Refused(exe) => scan.unreadable.push((pid, exe)),
            win::Read::Unreadable(exe) => again.push((pid, exe)),
        }
    }
    if again.is_empty() {
        return scan;
    }
    let pids: Vec<u32> = again.iter().map(|&(pid, _)| pid).collect();
    let mut first_exe: std::collections::HashMap<u32, Option<PathBuf>> =
        again.into_iter().collect();
    for (pid, read) in pass(&pids) {
        match read {
            win::Read::User(found) => scan.users.push(found),
            win::Read::None | win::Read::Exited => {}
            win::Read::Unreadable(exe) | win::Read::Refused(exe) => {
                let exe = exe.or_else(|| first_exe.remove(&pid).flatten());
                scan.unreadable.push((pid, exe));
            }
        }
    }
    scan
}

/// See the macOS twin.
#[cfg(windows)]
#[must_use]
pub fn exe_of(pid: u32) -> Exe {
    win::exe_of(pid)
}

#[cfg(windows)]
mod win {
    use super::{Exe, PathBuf, PathUse};
    use crate::win::ffi;
    use std::collections::HashMap;
    use std::os::windows::ffi::OsStringExt as _;
    use std::os::windows::io::{AsRawHandle as _, BorrowedHandle};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError, mpsc};
    use std::time::Duration;

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const PROCESS_VM_READ: u32 = 0x0010;
    const PROCESS_DUP_HANDLE: u32 = 0x0040;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const LIMITED: u32 = PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE;
    const READ_ALL: u32 = LIMITED | PROCESS_VM_READ | PROCESS_DUP_HANDLE;
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const ERROR_INVALID_HANDLE: u32 = 6;
    const WAIT_OBJECT_0: u32 = 0;
    const SYSTEM_EXTENDED_HANDLE_INFORMATION: u32 = 64;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = i32::from_ne_bytes(0xC000_0004u32.to_ne_bytes());
    const LIST_MODULES_ALL: u32 = 0x03;
    const FILE_TYPE_UNKNOWN: u32 = 0x0000;
    const FILE_TYPE_DISK: u32 = 0x0001;
    const VOLUME_NAME_DOS: u32 = 0x0000;
    const DUPLICATE_SAME_ACCESS: u32 = 0x0002;

    const NAME_DEADLINE: Duration = Duration::from_secs(1);

    /// What one process read found.
    pub(super) enum Read {
        User(PathUse),
        None,
        Exited,
        Unreadable(Option<PathBuf>),
        Refused(Option<PathBuf>),
    }

    struct Handle(ffi::RawHandle);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a live handle this module opened.
            let _ = unsafe { ffi::CloseHandle(self.0) };
        }
    }
    impl Handle {
        fn borrowed(&self) -> BorrowedHandle<'_> {
            // SAFETY: `self.0` is a live handle closed only when this `Handle` drops.
            unsafe { BorrowedHandle::borrow_raw(self.0 as std::os::windows::io::RawHandle) }
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Refusal {
        Gone,
        Denied,
    }

    fn open(pid: u32, access: u32) -> Result<Handle, Refusal> {
        // SAFETY: a plain open by pid; the handle is owned by `Handle`.
        let h = unsafe { ffi::OpenProcess(access, 0, pid) };
        if h != 0 {
            return Ok(Handle(h));
        }
        // SAFETY: a thread-local error getter.
        if unsafe { ffi::GetLastError() } == ERROR_INVALID_PARAMETER {
            Err(Refusal::Gone)
        } else {
            Err(Refusal::Denied)
        }
    }

    fn open_limited(pid: u32) -> Result<Handle, Refusal> {
        match open(pid, LIMITED) {
            Err(Refusal::Denied) => open(pid, PROCESS_QUERY_LIMITED_INFORMATION),
            other => other,
        }
    }

    fn exited(h: &Handle) -> bool {
        // SAFETY: `h.0` is a live process handle this module opened.
        let state = unsafe { ffi::WaitForSingleObject(h.0, 0) };
        state == WAIT_OBJECT_0
    }

    /// One witness's path as [`placed`] placed it against the directories.
    pub(super) struct Placed {
        pub(super) at: PathBuf,
        pub(super) under: Option<bool>,
    }

    fn note(placed: Placed, unreadable: &mut bool) -> Option<PathBuf> {
        match placed.under {
            Some(true) => Some(placed.at),
            Some(false) => None,
            None => {
                *unreadable = true;
                None
            }
        }
    }

    /// The first use `pid` makes of a path under `dirs`, each placed by `table` ([`placed`]).
    pub(super) fn uses(
        pid: u32,
        dirs: &[PathBuf],
        table: &aterm_sysprobe::volumes::Table,
        handles: Option<&[usize]>,
        namer: &mut Namer,
    ) -> Read {
        let handle = match open(pid, READ_ALL) {
            Ok(h) => Arc::new(h),
            Err(Refusal::Gone) => return Read::Exited,
            Err(Refusal::Denied) => return refused(pid),
        };
        if exited(&handle) {
            return Read::Exited;
        }
        let hit = |how: &'static str, path: PathBuf| Read::User(PathUse { pid, how, path });
        let mut unreadable = false;

        let image = image_path(&handle);
        match &image {
            Some(p) => {
                if let Some(at) = note(placed(p, dirs, table, namer), &mut unreadable) {
                    return hit("exe", at);
                }
            }
            None => unreadable = true,
        }
        match read_cwd(&handle) {
            Some(cwd) => {
                if let Some(at) = note(placed(&cwd, dirs, table, namer), &mut unreadable) {
                    return hit("cwd", at);
                }
            }
            None => unreadable = true,
        }
        match modules(&handle) {
            Some((mods, complete)) => {
                for (base, m) in &mods {
                    let recorded = placed(m, dirs, table, namer);
                    match recorded.under {
                        Some(true) => return hit("exe", recorded.at),
                        None => unreadable = true,
                        Some(false) => {
                            let query = Query::Mapped(Arc::clone(&handle), *base);
                            let mapped = match namer.name((pid, Of::Module(*base)), query) {
                                HandlePath::At(kernel) => on_volume(&kernel, table),
                                HandlePath::NoFile | HandlePath::Failed => None,
                            };
                            match mapped {
                                Some(file) => {
                                    let file = placed(&file, dirs, table, namer);
                                    if let Some(at) = note(file, &mut unreadable) {
                                        return hit("exe", at);
                                    }
                                }
                                None => unreadable = true,
                            }
                        }
                    }
                }
                if !complete {
                    unreadable = true;
                }
            }
            None => unreadable = true,
        }
        match handles {
            Some(handles) => {
                for &value in handles {
                    let path = match duplicate(&handle, value) {
                        Dup::Of(dup) => namer.name((pid, Of::Handle(value)), Query::Handle(dup)),
                        Dup::Closed => HandlePath::NoFile,
                        Dup::Failed => HandlePath::Failed,
                    };
                    match path {
                        HandlePath::At(path) => {
                            if let Some(at) =
                                note(placed(&path, dirs, table, namer), &mut unreadable)
                            {
                                return hit("fd", at);
                            }
                        }
                        HandlePath::NoFile => {}
                        HandlePath::Failed => unreadable = true,
                    }
                }
            }
            None => unreadable = true,
        }

        if !unreadable {
            Read::None
        } else if exited(&handle) {
            // It exited during the read: it held nothing.
            Read::Exited
        } else {
            Read::Unreadable(image)
        }
    }

    fn refused(pid: u32) -> Read {
        match open_limited(pid) {
            Ok(h) if exited(&h) => Read::Exited,
            Ok(h) => Read::Refused(image_path(&h)),
            Err(Refusal::Gone) => Read::Exited,
            Err(Refusal::Denied) => Read::Refused(None),
        }
    }

    /// [`exe_of`](super::exe_of).
    pub(super) fn exe_of(pid: u32) -> Exe {
        match open_limited(pid) {
            Ok(h) if exited(&h) => Exe::Exited,
            Ok(h) => image_path(&h).map_or(Exe::Unnamed, Exe::At),
            Err(Refusal::Gone) => Exe::Exited,
            Err(Refusal::Denied) => Exe::Unnamed,
        }
    }

    fn image_path(h: &Handle) -> Option<PathBuf> {
        aterm_sysprobe::peb::image_of(h.borrowed())
    }

    fn modules(h: &Handle) -> Option<(Vec<(usize, PathBuf)>, bool)> {
        let mut need: u32 = 0;
        let mut cap = 1024usize;
        loop {
            let mut mods = vec![0usize; cap];
            let cb = u32::try_from(cap * std::mem::size_of::<usize>()).ok()?;
            // SAFETY: `h.0` is live; `mods` is `cb` writable bytes; `need` live.
            let ok = unsafe {
                ffi::K32EnumProcessModulesEx(
                    h.0,
                    mods.as_mut_ptr(),
                    cb,
                    &mut need,
                    LIST_MODULES_ALL,
                )
            };
            if ok == 0 {
                // A process exiting, or not yet initialized, refuses the module
                // list; the caller reads that as unknown unless it has exited.
                return None;
            }
            let got = need as usize / std::mem::size_of::<usize>();
            if got > cap {
                cap = got;
                continue;
            }
            let mut paths = Vec::with_capacity(got);
            let mut complete = true;
            let mut buf = vec![0u16; 32768];
            for &m in &mods[..got] {
                // SAFETY: `h.0` and module `m` are live; `buf` is its capacity.
                let n = unsafe {
                    ffi::K32GetModuleFileNameExW(h.0, m, buf.as_mut_ptr(), buf.len() as u32)
                };
                match buf.get(..n as usize) {
                    Some(name) if n != 0 && name.len() < buf.len() => {
                        paths.push((m, PathBuf::from(std::ffi::OsString::from_wide(name))));
                    }
                    _ => complete = false,
                }
            }
            return Some((paths, complete));
        }
    }

    fn mapped_name(process: &Handle, base: usize) -> HandlePath {
        let mut buf = vec![0u16; 32768];
        // SAFETY: `process.0` is a live handle with `READ_ALL` rights and `buf` has capacity for the UTF-16 path.
        let n = unsafe {
            ffi::K32GetMappedFileNameW(
                process.0,
                base as *const core::ffi::c_void,
                buf.as_mut_ptr(),
                buf.len() as u32,
            )
        };
        match buf.get(..n as usize) {
            Some(name) if n != 0 && name.len() < buf.len() => {
                HandlePath::At(PathBuf::from(std::ffi::OsString::from_wide(name)))
            }
            _ => HandlePath::Failed,
        }
    }

    /// Resolve a kernel device path (`\Device\HarddiskVolume…`) onto its mounted volume root.
    pub(super) fn on_volume(
        kernel: &std::path::Path,
        table: &aterm_sysprobe::volumes::Table,
    ) -> Option<PathBuf> {
        const DEVICES: &str = r"\Device\";
        let s = kernel.to_str()?;
        let rest = s
            .get(..DEVICES.len())
            .filter(|p| p.eq_ignore_ascii_case(DEVICES))
            .and_then(|_| s.get(DEVICES.len()..))?;
        let (device, below) = rest.split_once('\\').unwrap_or((rest, ""));
        if device.is_empty() {
            return None;
        }
        let device = format!("{DEVICES}{device}");
        let mount = table.mounts.iter().find(|m| {
            m.fstype != aterm_sysprobe::volumes::SUBST && m.from.eq_ignore_ascii_case(&device)
        })?;
        let on = mount.on.to_str()?.trim_end_matches('\\');
        Some(PathBuf::from(format!(r"\\?\{on}\{below}")))
    }

    fn read_cwd(h: &Handle) -> Option<PathBuf> {
        aterm_sysprobe::peb::cwd_of(h.borrowed())
    }

    enum HandlePath {
        At(PathBuf),
        NoFile,
        Failed,
    }

    enum Dup {
        Of(Handle),
        Closed,
        Failed,
    }

    fn duplicate(h: &Handle, value: usize) -> Dup {
        let mut dup: ffi::RawHandle = 0;
        // SAFETY: `DuplicateHandle` duplicates `value` from `h` into this process, owned by the returned `Handle`.
        let ok = unsafe {
            ffi::DuplicateHandle(
                h.0,
                value as ffi::RawHandle,
                ffi::GetCurrentProcess(),
                &mut dup,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok != 0 && dup != 0 {
            return Dup::Of(Handle(dup));
        }
        // SAFETY: a thread-local error getter, read right after the call.
        if ok == 0 && unsafe { ffi::GetLastError() } == ERROR_INVALID_HANDLE {
            Dup::Closed
        } else {
            Dup::Failed
        }
    }

    fn handle_path(dup: &Handle) -> HandlePath {
        // SAFETY: a thread-local error setter.
        unsafe { ffi::SetLastError(0) };
        // SAFETY: `dup.0` is the live duplicate.
        let kind = unsafe { ffi::GetFileType(dup.0) };
        if kind != FILE_TYPE_DISK {
            // SAFETY: a thread-local error getter.
            let failed = kind == FILE_TYPE_UNKNOWN && unsafe { ffi::GetLastError() } != 0;
            return if failed {
                HandlePath::Failed
            } else {
                HandlePath::NoFile
            };
        }
        let mut buf = vec![0u16; 32768];
        // SAFETY: `dup.0` is a live disk handle and `buf` holds at least `buf.len()` UTF-16 units.
        let n = unsafe {
            ffi::GetFinalPathNameByHandleW(
                dup.0,
                buf.as_mut_ptr(),
                buf.len() as u32,
                VOLUME_NAME_DOS,
            )
        };
        match buf.get(..n as usize) {
            Some(path) if n != 0 => {
                HandlePath::At(PathBuf::from(std::ffi::OsString::from_wide(path)))
            }
            _ if object_name(dup).is_some_and(|name| disk_device_itself(&name)) => {
                HandlePath::NoFile
            }
            _ => HandlePath::Failed,
        }
    }

    fn object_name(h: &Handle) -> Option<String> {
        const OBJECT_NAME_INFORMATION: u32 = 1;
        let mut buf = vec![0u64; 8192];
        let bytes = u32::try_from(buf.len() * 8).ok()?;
        let mut ret: u32 = 0;
        // SAFETY: `h.0` is a live handle, `buf` has `bytes` writable bytes, and `ret` is a valid out-pointer.
        let st = unsafe {
            ffi::NtQueryObject(
                h.0,
                OBJECT_NAME_INFORMATION,
                buf.as_mut_ptr().cast(),
                bytes,
                &mut ret,
            )
        };
        if st < 0 {
            return None;
        }
        // SAFETY: `NtQueryObject` wrote a valid aligned `UnicodeString` header at the start of `buf`.
        let name: ffi::UnicodeString = unsafe { std::ptr::read(buf.as_ptr().cast()) };
        let base = buf.as_ptr() as usize;
        let start = name.Buffer.checked_sub(base)?;
        let end = start.checked_add(usize::from(name.Length))?;
        if start < std::mem::size_of::<ffi::UnicodeString>()
            || end > bytes as usize
            || start & 1 != 0
            || name.Length & 1 != 0
        {
            return None;
        }
        // SAFETY: `buf` is `bytes` long and `start..end` lies inside it.
        let raw = unsafe {
            std::slice::from_raw_parts(buf.as_ptr().cast::<u8>().add(start), end - start)
        };
        let wide: Vec<u16> = raw
            .chunks_exact(2)
            .map(|c| u16::from_ne_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&wide).ok()
    }

    /// Is `name`, a file object's kernel name, a disk device itself?
    pub(super) fn disk_device_itself(name: &str) -> bool {
        let numbered = |s: &str, prefix: &str| {
            s.strip_prefix(prefix)
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        };
        let Some(rest) = name.strip_prefix(r"\Device\") else {
            return false;
        };
        let mut parts = rest.split('\\');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(one), None, None) => numbered(one, "HarddiskVolume") || numbered(one, "CdRom"),
            (Some(disk), Some(part), None) => {
                numbered(disk, "Harddisk") && (numbered(part, "DR") || numbered(part, "Partition"))
            }
            _ => false,
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Of {
        Handle(usize),
        Module(usize),
        Spelling(u64),
    }

    const NO_PROCESS: u32 = 0;

    fn spelling_key(path: &std::path::Path) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::hash::DefaultHasher::new();
        path.hash(&mut hasher);
        hasher.finish()
    }

    static STUCK: Mutex<Vec<(u32, Of)>> = Mutex::new(Vec::new());

    #[cfg(test)]
    fn stuck() -> std::sync::MutexGuard<'static, Vec<(u32, Of)>> {
        STUCK.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many of `pid`'s queries still block an abandoned helper.
    #[cfg(test)]
    pub(super) fn stuck_for(pid: u32) -> usize {
        stuck().iter().filter(|(p, _)| *p == pid).count()
    }

    /// `f`'s answer, run while the long spelling of `through` is marked stuck in [`STUCK`], as one whose helper outlived its deadline is.
    #[cfg(test)]
    pub(super) fn with_spelling_stuck<R>(through: &std::path::Path, f: impl FnOnce() -> R) -> R {
        struct Unstick((u32, Of));
        impl Drop for Unstick {
            fn drop(&mut self) {
                let mut stuck = stuck();
                if let Some(at) = stuck.iter().position(|k| *k == self.0) {
                    stuck.swap_remove(at);
                }
            }
        }
        let key = (NO_PROCESS, Of::Spelling(spelling_key(through)));
        stuck().push(key);
        let _unstick = Unstick(key);
        f()
    }

    /// Return whether the [`Namer`] helper thread keeps system error dialogs suppressed.
    #[cfg(test)]
    pub(super) fn helper_errors_quiet(deadline: Duration) -> Option<bool> {
        fn probe() -> HandlePath {
            HandlePath::At(PathBuf::from(aterm_sysprobe::errors_quiet().to_string()))
        }
        let mut namer = Namer::within(deadline);
        match namer.name((u32::MAX, Of::Handle(usize::MAX)), Query::Probe(probe)) {
            HandlePath::At(said) => said.to_str()?.parse().ok(),
            HandlePath::NoFile | HandlePath::Failed => None,
        }
    }

    /// Names one scan's file handles ([`handle_path`]) and mapped modules ([`mapped_name`]).
    pub(super) struct Namer {
        helper: Option<Helper>,
        deadline: Duration,
    }

    impl Default for Namer {
        fn default() -> Self {
            Self::within(NAME_DEADLINE)
        }
    }

    struct Helper {
        jobs: mpsc::Sender<Job>,
        answers: mpsc::Receiver<HandlePath>,
        abandoned: Arc<AtomicBool>,
    }

    enum Query {
        Handle(Handle),
        Mapped(Arc<Handle>, usize),
        Spell(PathBuf),
        #[cfg(test)]
        Probe(fn() -> HandlePath),
    }

    struct Job {
        key: (u32, Of),
        query: Query,
    }

    impl Namer {
        /// A namer whose queries each wait at most `deadline`.
        pub(super) fn within(deadline: Duration) -> Self {
            Self {
                helper: None,
                deadline,
            }
        }

        fn name(&mut self, key: (u32, Of), query: Query) -> HandlePath {
            crate::progress::tick();
            if STUCK
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&key)
            {
                return HandlePath::Failed;
            }
            let Some(helper) = self.helper.take().or_else(Helper::start) else {
                return HandlePath::Failed;
            };
            if helper.jobs.send(Job { key, query }).is_err() {
                return HandlePath::Failed;
            }
            match helper.answers.recv_timeout(self.deadline) {
                Ok(path) => {
                    self.helper = Some(helper);
                    path
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => HandlePath::Failed,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let mut stuck = STUCK.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Ok(path) = helper.answers.try_recv() {
                        drop(stuck);
                        self.helper = Some(helper);
                        return path;
                    }
                    helper.abandoned.store(true, Ordering::Relaxed);
                    stuck.push(key);
                    HandlePath::Failed
                }
            }
        }

        fn long_name(&mut self, through: &std::path::Path) -> Option<PathBuf> {
            let key = (NO_PROCESS, Of::Spelling(spelling_key(through)));
            match self.name(key, Query::Spell(through.to_path_buf())) {
                HandlePath::At(long) => Some(long),
                HandlePath::NoFile | HandlePath::Failed => None,
            }
        }
    }

    impl Helper {
        fn start() -> Option<Self> {
            let (jobs, job_rx) = mpsc::channel::<Job>();
            let (answer_tx, answers) = mpsc::channel();
            let abandoned = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&abandoned);
            std::thread::Builder::new()
                .name("aterm-handle-name".into())
                .spawn(move || answer(&job_rx, &answer_tx, &flag))
                .ok()?;
            Some(Self {
                jobs,
                answers,
                abandoned,
            })
        }
    }

    fn answer(
        jobs: &mpsc::Receiver<Job>,
        answers: &mpsc::Sender<HandlePath>,
        abandoned: &AtomicBool,
    ) {
        let _quiet = aterm_sysprobe::quiet_errors();
        while let Ok(Job { key, query }) = jobs.recv() {
            let path = match query {
                Query::Handle(dup) => handle_path(&dup),
                Query::Mapped(process, base) => mapped_name(&process, base),
                Query::Spell(through) => aterm_sysprobe::volumes::long_name(&through)
                    .map_or(HandlePath::Failed, HandlePath::At),
                #[cfg(test)]
                Query::Probe(probe) => probe(),
            };
            let mut stuck = STUCK.lock().unwrap_or_else(PoisonError::into_inner);
            if abandoned.load(Ordering::Relaxed) {
                if let Some(at) = stuck.iter().position(|k| *k == key) {
                    stuck.swap_remove(at);
                }
                return;
            }
            let _ = answers.send(path);
        }
    }

    /// Every process's open handles of the `File` object type.
    pub(super) fn file_handles() -> Option<HashMap<u32, Vec<usize>>> {
        let probe = std::fs::File::open("NUL").ok()?;
        let own = (std::process::id() as usize, probe.as_raw_handle() as usize);
        let entries = handle_snapshot()?;
        drop(probe);
        let file_type = entries
            .iter()
            .find(|e| (e.UniqueProcessId, e.HandleValue) == own)?
            .ObjectTypeIndex;
        let mut map: HashMap<u32, Vec<usize>> = HashMap::new();
        for e in entries.iter().filter(|e| e.ObjectTypeIndex == file_type) {
            if let Ok(pid) = u32::try_from(e.UniqueProcessId) {
                map.entry(pid).or_default().push(e.HandleValue);
            }
        }
        Some(map)
    }

    fn handle_snapshot() -> Option<Vec<ffi::SystemHandleEntryEx>> {
        let mut cap = 1 << 20; // a megabyte, grown on mismatch
        let entry = std::mem::size_of::<ffi::SystemHandleEntryEx>();
        let header = 2 * std::mem::size_of::<usize>();
        for _ in 0..8 {
            let mut buf = vec![0u8; cap];
            let mut ret: u32 = 0;
            let len = u32::try_from(cap).ok()?;
            // SAFETY: `buf` is `len` writable bytes; `ret` is a live local.
            let st = unsafe {
                ffi::NtQuerySystemInformation(
                    SYSTEM_EXTENDED_HANDLE_INFORMATION,
                    buf.as_mut_ptr().cast(),
                    len,
                    &mut ret,
                )
            };
            if st == STATUS_INFO_LENGTH_MISMATCH {
                cap = (ret as usize).max(cap * 2).max(cap + entry * 4096);
                continue;
            }
            if st < 0 {
                return None;
            }
            let count = buf
                .get(..std::mem::size_of::<usize>())
                .and_then(|b| <[u8; std::mem::size_of::<usize>()]>::try_from(b).ok())
                .map(usize::from_ne_bytes)?;
            let end = count.checked_mul(entry)?.checked_add(header)?;
            let body = buf.get(header..end)?;
            return Some(
                body.chunks_exact(entry)
                    // SAFETY: each chunk is exactly one entry's bytes, copied out.
                    .map(|slot| unsafe { std::ptr::read_unaligned(slot.as_ptr().cast()) })
                    .collect(),
            );
        }
        None
    }

    /// Is the kernel's `path` at or under one of `dirs` (each canonical, so a verbatim `\\?\C:\...` spelling).
    pub(super) fn placed(
        path: &std::path::Path,
        dirs: &[PathBuf],
        table: &aterm_sysprobe::volumes::Table,
        namer: &mut Namer,
    ) -> Placed {
        let normalized = win32_normalized(path);
        let at = table
            .long_spelling_by(&normalized, |through| namer.long_name(through))
            .into_owned();
        let mut unplaced = false;
        for d in dirs {
            match table.is_under(&at, d) {
                Some(true) => {
                    return Placed {
                        at,
                        under: Some(true),
                    };
                }
                Some(false) => {}
                None => unplaced = true,
            }
        }
        Placed {
            at,
            under: (!unplaced).then_some(false),
        }
    }

    /// `path` as Win32 normalizes a path that is not verbatim before any file system sees it.
    pub(super) fn win32_normalized(
        path: &std::path::Path,
    ) -> std::borrow::Cow<'_, std::path::Path> {
        use std::borrow::Cow;
        let Some(s) = path.to_str() else {
            return Cow::Borrowed(path);
        };
        if s.starts_with(r"\\?\") || s.starts_with(r"\??\") {
            return Cow::Borrowed(path);
        }
        let s = s.replace('/', r"\");
        let (root, rest) = if let Some(unc) = s.strip_prefix(r"\\") {
            let mut parts = unc.splitn(3, '\\');
            let (Some(server), Some(share)) = (parts.next(), parts.next()) else {
                return Cow::Borrowed(path);
            };
            if matches!(server, "" | "." | "?") || share.is_empty() {
                return Cow::Borrowed(path);
            }
            (format!(r"\\{server}\{share}"), parts.next().unwrap_or(""))
        } else {
            let b = s.as_bytes();
            let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
            if !drive || b.get(2).is_some_and(|&c| c != b'\\') {
                return Cow::Borrowed(path);
            }
            (s[..2].to_owned(), s.get(3..).unwrap_or(""))
        };
        let mut names: Vec<&str> = Vec::new();
        for name in rest.split('\\') {
            match name {
                "" | "." => {}
                ".." => {
                    names.pop();
                }
                n => names.push(n),
            }
        }
        let mut out = root;
        if names.is_empty() {
            out.push('\\');
        }
        for name in names {
            out.push('\\');
            out.push_str(name);
        }
        Cow::Owned(PathBuf::from(out))
    }
}

/// See the macOS twin.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
pub fn scan_path_users(dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    scan_path_users_of(&[], dirs, skip)
}

/// See the macOS twin.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
pub fn scan_path_users_of(_pids: &[u32], _dirs: &[PathBuf], _skip: &[u32]) -> io::Result<PathScan> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the path-user scan reads libproc (macOS) or /proc (Linux)",
    ))
}

#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn write_open_files(pid: u32) -> io::Result<Vec<PathBuf>> {
    mac::writes(pid)
        .map_err(|_| io::Error::other(format!("pid {pid}'s descriptor table is unreadable")))
}

/// See the macOS twin.
#[cfg(target_os = "linux")]
pub fn write_open_files(pid: u32) -> io::Result<Vec<PathBuf>> {
    const O_ACCMODE: u32 = 0o3;
    let proc_dir = PathBuf::from(format!("/proc/{pid}"));
    let rd = match std::fs::read_dir(proc_dir.join("fd")) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Ok(file) = ({
            use std::os::unix::fs::OpenOptionsExt as _;
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(proc_dir.join("fdinfo").join(&name))
        }) else {
            // Distinct branch.
            continue; // closed since the listing
        };
        let mut info = String::with_capacity(256);
        if std::io::Read::read_to_string(&mut std::io::Read::take(file, 64 * 1024), &mut info)
            .is_err()
        {
            continue;
        }
        let flags = info
            .lines()
            .find_map(|l| l.strip_prefix("flags:"))
            .map(str::trim)
            .filter(|v| !v.is_empty() && v.bytes().all(|b| matches!(b, b'0'..=b'7')))
            .and_then(|v| u32::from_str_radix(v, 8).ok());
        if !flags.is_some_and(|f| matches!(f & O_ACCMODE, 1 | 2)) {
            continue;
        }
        let Ok(path) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let deleted = path.as_os_str().as_encoded_bytes().ends_with(b" (deleted)");
        if path.is_absolute()
            && !deleted
            && !["/dev", "/proc", "/sys"]
                .iter()
                .any(|d| path.starts_with(d))
        {
            out.push(path);
        }
    }
    Ok(out)
}

/// See the macOS twin.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub fn write_open_files(_pid: u32) -> io::Result<Vec<PathBuf>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "a process's open files are read through libproc (macOS) or /proc (Linux)",
    ))
}

#[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
#[path = "holders_paths_writes_tests.rs"]
mod writes_tests;

#[cfg(all(any(test, debug_assertions), target_vendor = "apple"))]
fn running(child: &crate::spawnfd::Child, exe: &str) -> u32 {
    let pid = child.id();
    let start = std::time::Instant::now();
    while executable(pid).as_deref() != Some(std::path::Path::new(exe)) {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(60),
            "the child never ran {exe}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    pid
}

#[cfg(all(debug_assertions, target_vendor = "apple"))]
mod dmg_acceptance {
    use super::*;
    #[cfg(target_vendor = "apple")]
    const HDIUTIL: &str = crate::os_path!("hdiutil");

    #[cfg(target_vendor = "apple")]
    fn detach(dev: &str) -> io::Result<std::process::Output> {
        crate::spawnfd::command(HDIUTIL)
            .args(["detach", "-force", dev])
            .stdin(crate::spawnfd::Stdio::null())
            .output()
    }

    #[cfg(target_vendor = "apple")]
    struct Volume {
        dev: String,
    }

    #[cfg(target_vendor = "apple")]
    impl Volume {
        fn attach(image: &std::path::Path, mnt: &std::path::Path) -> std::io::Result<Self> {
            use crate::spawnfd::Stdio;
            let created = crate::spawnfd::command(HDIUTIL)
                .args(["create", "-size", "2m", "-fs", "HFS+"])
                .args(["-layout", "NONE", "-volname", "aterm-uds-test"])
                .arg(image)
                .stdin(Stdio::null())
                .output()?;
            if !created.status.success() {
                return Err(std::io::Error::other(format!(
                    "hdiutil create: {created:?}"
                )));
            }
            let attached = crate::spawnfd::command(HDIUTIL)
                .args(["attach", "-nobrowse", "-noverify", "-noautoopen"])
                .arg("-mountpoint")
                .arg(mnt)
                .arg(image)
                .stdin(Stdio::null())
                .output()?;
            if !attached.status.success() {
                return Err(std::io::Error::other(format!(
                    "hdiutil attach: {attached:?}"
                )));
            }
            let out = String::from_utf8_lossy(&attached.stdout);
            let dev = out
                .split_whitespace()
                .find(|w| w.starts_with("/dev/disk"))
                .unwrap_or_else(|| panic!("no device in {out:?}"));
            Ok(Self {
                dev: dev.to_owned(),
            })
        }

        fn detach_by_force(&mut self) -> std::process::Output {
            let out = detach(&self.dev).expect("hdiutil detach after a successful attach");
            if out.status.success() {
                self.dev.clear();
            }
            out
        }
    }

    #[cfg(target_vendor = "apple")]
    impl Drop for Volume {
        fn drop(&mut self) {
            if !self.dev.is_empty() {
                let _ = detach(&self.dev);
            }
        }
    }

    /// A child works at the root of a disk image's volume and holds a file elsewhere open.
    pub fn a_force_detached_volume_is_no_working_directory(base: &Path) -> std::io::Result<()> {
        use crate::spawnfd::Stdio;
        use std::os::unix::fs::MetadataExt as _;
        let me = std::process::id();
        let base = base.canonicalize()?;
        let (image, mnt, held) = (base.join("v.dmg"), base.join("mnt"), base.join("held"));
        std::fs::create_dir_all(&mnt).expect("the mount point");
        std::fs::create_dir_all(&held).expect("the held file's directory");
        let file = held.join("open.rlib");
        std::fs::write(&file, b"x").expect("a file to hold");
        let device = |p: &std::path::Path| std::fs::metadata(p).expect("stat").dev();
        let mut volume = Volume::attach(&image, &mnt)?;
        let mounted = device(&mnt) != device(&base);
        let mut child = crate::spawnfd::command("/bin/sh")
            .arg("-c")
            .arg("exec 3<\"$1\"; exec /bin/cat")
            .arg("sh")
            .arg(&file)
            .current_dir(&mnt)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("a child working on the volume");
        let pid = running(&child, "/bin/cat");
        let scan = |dir: &PathBuf| {
            scan_path_users_of(&[pid], std::slice::from_ref(dir), &[me]).expect("scan")
        };
        let before = scan(&mnt);
        let detached = volume.detach_by_force();
        let unmounted = device(&mnt) == device(&base);
        let read = mac::cwd(pid as i32);
        let after = scan(&mnt);
        let through_fd = scan(&held);
        let old = super::super::hook::reading_dead_as_unknown(|| scan(&mnt));
        let _ = child.kill();
        let _ = child.wait();
        drop(volume);

        assert!(mounted, "the image is mounted at {mnt:?}");
        assert_eq!(
            before.users,
            [PathUse {
                pid,
                how: "cwd",
                path: mnt.clone()
            }],
            "before the detach, a user through its working directory"
        );
        assert!(detached.status.success(), "detach -force: {detached:?}");
        assert!(unmounted, "the volume is gone from {mnt:?}");
        assert_eq!(
            read,
            Err(super::super::Failed::Dead),
            "its working directory reads ENOENT"
        );
        assert_eq!(
            (after.scanned, after.users, after.unreadable),
            (1, Vec::new(), Vec::new()),
            "read, and no user"
        );
        assert_eq!(
            through_fd.users,
            [PathUse {
                pid,
                how: "fd",
                path: file.clone()
            }],
            "read on, through its descriptors"
        );
        assert_eq!(
            old.unreadable.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            [pid],
            "the old reading: unreadable"
        );
        Ok(())
    }
}

#[cfg(all(debug_assertions, target_vendor = "apple"))]
pub use dmg_acceptance::a_force_detached_volume_is_no_working_directory;

#[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
#[path = "holders_paths_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "holders_paths_win_tests.rs"]
mod win_tests;

/// A SCAN SAYS IT IS STILL GOING AS IT GOES: nothing bounds how long one takes (it asks the kernel
/// about each mapped region of each process), and whoever waits on it reads silence as a hang.
#[cfg(all(test, any(target_vendor = "apple", target_os = "linux", windows)))]
#[path = "holders_paths_progress_tests.rs"]
mod progress_tests;
