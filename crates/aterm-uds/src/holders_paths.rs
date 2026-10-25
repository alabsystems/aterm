// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Open-file and working-directory path holder inspection across macOS, Linux, and Windows.
use std::io;
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
use std::path::Path;
use std::path::PathBuf;

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
    let dir = std::fs::File::open(format!("/proc/{}", std::process::id())).ok()?;
    // SAFETY: `dir` is open for the call and the name is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c"exe".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
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
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// See the macOS twin.
#[cfg(target_os = "linux")]
#[must_use]
pub fn exe_of(pid: u32) -> Exe {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStringExt as _;
    let gone = |e: &io::Error| matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ESRCH));
    let dir = match std::fs::File::open(format!("/proc/{pid}")) {
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
        let Ok(info) = std::fs::read_to_string(proc_dir.join("fdinfo").join(&name)) else {
            // Distinct branch.
            continue; // closed since the listing
        };
        let flags = info
            .lines()
            .find_map(|l| l.strip_prefix("flags:"))
            .and_then(|v| u32::from_str_radix(v.trim(), 8).ok());
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
mod writes_tests {
    use std::path::{Path, PathBuf};

    use super::write_open_files;

    const NO_SUCH_PID: u32 = 2_147_483_647;

    fn named(got: &[PathBuf], p: &Path) -> bool {
        got.iter().any(|g| {
            g == p
                || g.strip_prefix("/System/Volumes/Data")
                    .is_ok_and(|rel| Path::new("/").join(rel) == p)
        })
    }

    /// A process of this user's that became a set-user-ID program after the listing (`/bin/ps`
    /// run from a shell, measured refusing the disk sweep's scan under a `ps` loop) is outside
    /// the scan, not unread; `top` is one that stays to be read, handed to the scan as the
    /// listing would have before its exec. Negative control: without the guard it is unread.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_process_that_became_a_set_user_id_program_is_outside_the_path_scan() {
        // SAFETY: `geteuid` is a side-effect-free getter.
        let me = unsafe { super::super::ffi::geteuid() };
        assert_ne!(me, 0, "a scan as root reads every process");
        let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path().canonicalize().expect("canonical scratch dir");
        let mut top = crate::spawnfd::command("/usr/bin/top")
            .args(["-l", "0", "-s", "60"])
            .stdin(crate::spawnfd::Stdio::null())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn top");
        let pid = top.id();
        let started = std::time::Instant::now();
        while crate::process::read_short_info(i32::try_from(pid).expect("a pid"))
            .is_none_or(|info| info.pbsi_uid == me)
        {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "top never ran as its owner"
            );
            std::thread::yield_now();
        }
        let scan = super::scan_path_users_of(&[pid], std::slice::from_ref(&dir), &[]);
        let _ = top.kill();
        let _ = top.wait();
        let scan = scan.expect("scan");
        assert_eq!(scan.scanned, 1, "{scan:?}");
        assert!(
            scan.unreadable.is_empty(),
            "outside this user's scan: {scan:?}"
        );
        assert!(scan.users.is_empty(), "{scan:?}");
    }

    #[test]
    fn the_writes_reader_names_the_regular_files_open_for_writing_and_nothing_else() {
        let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path().canonicalize().expect("canonical scratch dir");
        let wo = dir.join("write-only.log");
        let rw = dir.join("read-write.db");
        let ro = dir.join("read-only.txt");
        std::fs::write(&ro, b"x").expect("seed the read-only file");
        let _w = std::fs::File::create(&wo).expect("open write-only");
        let _rw = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&rw)
            .expect("open read-write");
        let _r = std::fs::File::open(&ro).expect("open read-only");
        let _null = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .expect("open /dev/null for writing");
        let got = write_open_files(std::process::id()).expect("own table");
        assert!(named(&got, &wo), "{wo:?} not in {got:?}");
        assert!(named(&got, &rw), "{rw:?} not in {got:?}");
        assert!(!named(&got, &ro), "a read-only open is no write: {got:?}");
        assert!(
            !got.iter().any(|g| g.starts_with("/dev")),
            "a device is no regular file: {got:?}"
        );
    }

    #[test]
    fn a_process_that_does_not_exist_holds_no_file() {
        assert_eq!(write_open_files(NO_SUCH_PID).ok(), Some(Vec::new()));
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_open_files_reader_names_a_read_only_file_and_refuses_an_exited_pid() {
        let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path().canonicalize().expect("canonical scratch dir");
        let lock = dir.join("held.lock");
        std::fs::write(&lock, b"").expect("seed the lock");
        let _held = std::fs::File::open(&lock).expect("open read-only");
        let got = super::open_files(std::process::id()).expect("own table");
        assert!(named(&got, &lock), "{lock:?} not in {got:?}");
        let gone = super::open_files(NO_SUCH_PID).map_err(|e| e.kind());
        assert_eq!(gone, Err(std::io::ErrorKind::NotFound));
    }
}

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
mod tests {
    #[cfg(target_vendor = "apple")]
    use super::running;
    use super::{PathBuf, placed, scan_path_users};
    use std::time::{Duration, Instant};

    const HANG: Duration = Duration::from_secs(60);

    /// A new directory of this run's own: never one an earlier run with this pid kept, which may hold a copy that never showed it ran.
    fn fresh(name: &str) -> PathBuf {
        let d = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-uds-paths-{name}-"))
            .tempdir()
            .expect("mkdir")
            .keep();
        std::fs::canonicalize(&d).expect("canonical")
    }

    fn until(dirs: &[PathBuf], pid: u32, want: impl Fn(Option<&str>) -> bool) -> Option<String> {
        let start = Instant::now();
        loop {
            let scan = scan_path_users(dirs, &[std::process::id()]).expect("scan");
            let how = scan
                .users
                .iter()
                .find(|u| u.pid == pid)
                .map(|u| u.how.to_owned());
            if want(how.as_deref()) || start.elapsed() > HANG {
                return how;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_child_working_or_holding_a_file_under_the_directory_uses_it() {
        let dir = fresh("cwd");
        let deep = dir.join("debug/deps");
        std::fs::create_dir_all(&deep).expect("deps");
        let mut sleeper = crate::spawnfd::command("/bin/sleep")
            .arg("120")
            .current_dir(&deep)
            .spawn()
            .expect("sleep");
        let how = until(std::slice::from_ref(&dir), sleeper.id(), |h| h.is_some());
        let _ = sleeper.kill();
        let _ = sleeper.wait();
        assert_eq!(how.as_deref(), Some("cwd"));

        let file = dir.join("held.rlib");
        std::fs::write(&file, b"x").expect("rlib");
        let mut holder = crate::spawnfd::command("/bin/sh")
            .arg("-c")
            .arg("exec 3<\"$1\"; exec /bin/sleep 120")
            .arg("sh")
            .arg(&file)
            .current_dir("/")
            .spawn()
            .expect("holder");
        let how = until(std::slice::from_ref(&dir), holder.id(), |h| h == Some("fd"));
        let _ = holder.kill();
        let _ = holder.wait();
        assert_eq!(how.as_deref(), Some("fd"));

        let empty = fresh("nobody");
        let scan =
            scan_path_users(std::slice::from_ref(&empty), &[std::process::id()]).expect("scan");
        assert!(scan.users.is_empty(), "{:?}", scan.users);
        assert!(scan.scanned > 0);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_process_whose_executable_was_deleted_is_read_through_its_working_directory() {
        let dir = fresh("unlinked");
        let (bin, work) = (dir.join("catter"), dir.join("work"));
        std::fs::create_dir_all(&work).expect("work");
        crate::deadline::ad_hoc_copy(std::path::Path::new("/bin/cat"), &bin);
        let mut catter = crate::spawnfd::command(&bin)
            .current_dir(&work)
            .stdin(crate::spawnfd::Stdio::piped())
            .stdout(crate::spawnfd::Stdio::piped())
            .spawn()
            .expect("the copied cat");
        let to = crate::deadline::cat_echoes(&mut catter);
        assert_eq!(
            until(std::slice::from_ref(&dir), catter.id(), |h| h.is_some()).as_deref(),
            Some("exe"),
            "running from the directory, before the delete"
        );
        std::fs::remove_file(&bin).expect("delete the executable");
        let how = until(std::slice::from_ref(&dir), catter.id(), |h| {
            h == Some("cwd")
        });
        let scan =
            scan_path_users(std::slice::from_ref(&dir), &[std::process::id()]).expect("scan");
        let _ = catter.kill();
        let _ = catter.wait();
        drop(to);
        assert_eq!(how.as_deref(), Some("cwd"));
        assert!(
            !scan.unreadable.iter().any(|(pid, _)| *pid == catter.id()),
            "the deleted executable made it unreadable: {:?}",
            scan.unreadable
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const FORK_UNDER_A_TRANSLATOR: &str = "the kernel names the translator as the executable \
        of this process, of its un-exec'd fork and of every program the translator runs \
        (binfmt_misc), so a fork cannot be told from a child that exec'd another program; the \
        fork is kept as a user instead (asserted), and a real x86_64 CPU decides the skip";

    #[test]
    fn a_fork_of_this_process_that_has_not_execd_is_skipped_with_it() {
        unsafe extern "C" {
            fn fork() -> i32;
            fn pause() -> i32;
            fn _exit(code: i32) -> !;
            fn kill(pid: i32, sig: i32) -> i32;
            fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
        }
        let dir = fresh("fork");
        let held = std::fs::File::open(&dir).expect("a handle on the directory");
        // SAFETY: the child only calls async-signal-safe `pause`/`_exit`.
        let child = unsafe { fork() };
        if child == 0 {
            // SAFETY: as above.
            unsafe {
                pause();
                _exit(0)
            }
        }
        assert!(child > 0, "fork");
        let me = std::process::id();
        let child_pid = u32::try_from(child).expect("pid");
        let skipped = scan_path_users(std::slice::from_ref(&dir), &[me]).expect("scan");
        let unskipped = scan_path_users(std::slice::from_ref(&dir), &[]).expect("scan");
        // SAFETY: our own child; SIGKILL, then reaped.
        unsafe {
            kill(child, 9);
            waitpid(child, std::ptr::null_mut(), 0);
        }
        drop(held);
        let kept = skipped.users.iter().any(|u| u.pid == child_pid);
        if let Some(translator) = crate::spawnfd::tests::translator() {
            assert!(
                kept,
                "under {translator} the fork is kept: {:?}",
                skipped.users
            );
            println!(
                "NOT RUN under {translator}: holders::paths::tests::\
                 a_fork_of_this_process_that_has_not_execd_is_skipped_with_it: the skip: \
                 {FORK_UNDER_A_TRANSLATOR}"
            );
        } else {
            assert!(!kept, "{:?}", skipped.users);
        }
        assert!(
            unskipped.users.iter().any(|u| u.pid == child_pid),
            "{:?}",
            unskipped.users
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const PARK_ENV: &str = "ATERM_UDS_PATHS_PARK";

    const PARKED: &str = "parked";

    #[test]
    fn a_child_that_execd_another_program_is_a_user_with_this_process_skipped() {
        const NAME: &str = "holders::paths::tests::\
            a_child_that_execd_another_program_is_a_user_with_this_process_skipped";
        if std::env::var_os(PARK_ENV).is_some() {
            println!("{PARKED}");
            let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0u8; 1]);
            return;
        }
        let dir = fresh("execd");
        let bin_dir = fresh("execd-bin");
        let bin = bin_dir.join("other");
        std::fs::copy(std::env::current_exe().expect("the test binary"), &bin)
            .expect("a copy of the test binary");
        let mut other = crate::spawnfd::command(&bin)
            .args([
                "--exact",
                NAME,
                "--nocapture",
                "--quiet",
                "--test-threads=1",
            ])
            .env(PARK_ENV, "1")
            .current_dir(&dir)
            .stdin(crate::spawnfd::Stdio::piped())
            .stdout(crate::spawnfd::Stdio::piped())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("the copy");
        crate::deadline::prints(&mut other, PARKED, crate::deadline::FIRST_EXEC_HANG);
        let how = until(std::slice::from_ref(&dir), other.id(), |h| h.is_some());
        let (mine, its) = (
            super::executable(std::process::id()),
            super::executable(other.id()),
        );
        drop(other.stdin.take());
        let _ = other.kill();
        let _ = other.wait();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&bin_dir);
        if let Some(translator) = crate::spawnfd::tests::translator() {
            assert_eq!(
                mine, its,
                "PRECONDITION: under {translator} both run the translator"
            );
        } else {
            assert_ne!(
                mine, its,
                "PRECONDITION: natively the copy is another executable"
            );
        }
        assert_eq!(
            how.as_deref(),
            Some("cwd"),
            "this process's {mine:?}, its {its:?}"
        );
    }

    #[cfg(target_vendor = "apple")]
    fn exec_done(child: &crate::spawnfd::Child) -> u32 {
        running(child, "/bin/sleep")
    }

    #[cfg(target_vendor = "apple")]
    fn bare_child(cwd: &std::path::Path) -> (crate::spawnfd::Child, i32) {
        const WUNTRACED: i32 = 2;
        let child = crate::spawnfd::command("/bin/sh")
            .args(["-c", "exec 0<&- 1>&- 2>&-; kill -STOP $$"])
            .current_dir(cwd)
            .stdin(crate::spawnfd::Stdio::null())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn the bare shell");
        let pid = i32::try_from(child.id()).expect("pid");
        let mut status = 0;
        // SAFETY: `waitpid(WUNTRACED)` waits for our child to stop without reaping it.
        let got = unsafe { proc_ffi::waitpid(pid, &raw mut status, WUNTRACED) };
        assert_eq!(got, pid, "waitpid");
        assert_eq!(status & 0xff, 0x7f, "stopped, not exited: {status:#x}");
        (child, pid)
    }

    #[cfg(target_vendor = "apple")]
    fn reap(mut child: crate::spawnfd::Child) {
        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(target_vendor = "apple")]
    mod proc_ffi {
        unsafe extern "C" {
            pub fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
            pub fn _exit(code: i32) -> !;
            pub fn grantpt(fd: i32) -> i32;
            pub fn unlockpt(fd: i32) -> i32;
            pub fn ioctl(fd: i32, request: core::ffi::c_ulong, ...) -> i32;
            pub fn revoke(path: *const core::ffi::c_char) -> i32;
        }
    }

    #[cfg(target_vendor = "apple")]
    fn a_pty() -> (std::os::fd::OwnedFd, PathBuf) {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::ffi::OsStrExt as _;
        const TIOCPTYGNAME: core::ffi::c_ulong = 0x4080_7453;
        let (master, _) = super::super::tests::a_master_at(230);
        let m = master.as_raw_fd();
        // SAFETY: `m` is this test's open master.
        assert_eq!(unsafe { proc_ffi::grantpt(m) }, 0, "grantpt");
        // SAFETY: as above.
        assert_eq!(unsafe { proc_ffi::unlockpt(m) }, 0, "unlockpt");
        let mut name = [0u8; 128];
        // SAFETY: TIOCPTYGNAME writes at most 128 bytes, NUL-terminated.
        let rc = unsafe { proc_ffi::ioctl(m, TIOCPTYGNAME, name.as_mut_ptr()) };
        assert_eq!(rc, 0, "TIOCPTYGNAME");
        let len = name.iter().position(|b| *b == 0).expect("NUL-terminated");
        let slave = std::ffi::OsStr::from_bytes(&name[..len]);
        (master, PathBuf::from(slave))
    }

    /// A PTY's master is named from its slave's number ([`super::super::pty_master_rdev`],
    /// what a process's `e_tdev` gives), and a scan of that number finds this process, which
    /// holds the master. NEGATIVE CONTROL: the slave's own number is another device, so a
    /// mapping that handed it back unchanged (a scan of the slave) fails the first assertion.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_ptys_master_is_named_from_its_slave() {
        use std::os::unix::fs::MetadataExt as _;
        const DEV: u64 = 0xffff_ffff;
        let me = std::process::id();
        let (master, slave) = a_pty();
        let slave_rdev = std::fs::metadata(&slave).expect("stat the slave").rdev() & DEV;
        let master_rdev = std::fs::File::from(master.try_clone().expect("dup"))
            .metadata()
            .expect("fstat the master")
            .rdev()
            & DEV;
        let named = super::super::pty_master_rdev(slave_rdev).expect("/dev/ptmx reads");
        let held = super::super::scan_rdev_holders(named, &[]).expect("scan");
        drop(master);
        assert_eq!(named, master_rdev, "the master's number, from the slave's");
        assert_ne!(slave_rdev, master_rdev, "the slave is another device");
        assert!(held.holds(me), "this process holds the master: {held:?}");
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_descriptor_on_a_revoked_terminal_holds_nothing() {
        use crate::spawnfd::Stdio;
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::MetadataExt as _;
        let me = std::process::id();
        let (master, slave) = a_pty();
        let rdev = std::fs::metadata(&slave).expect("stat the slave").rdev();
        let dirs = std::slice::from_ref(&slave);
        let mut child = crate::spawnfd::command("/bin/sh")
            .arg("-c")
            .arg("exec 3<>\"$1\"; exec /bin/sleep 600")
            .arg("sh")
            .arg(&slave)
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("a holder of the slave");
        let pid = exec_done(&child);
        let n = pid as i32;
        let before = (
            super::scan_path_users_of(&[pid], dirs, &[me]).expect("scan"),
            super::super::process_holds_rdev(pid, rdev),
        );
        let path = std::ffi::CString::new(slave.as_os_str().as_bytes()).expect("a path");
        // SAFETY: a NUL-terminated path, to the slave of this test's own PTY.
        let revoked = unsafe { proc_ffi::revoke(path.as_ptr()) };
        let revoke_err = std::io::Error::last_os_error();
        let fds = super::super::list_fds(n);
        let read = super::super::vnode_of(n, 3, super::super::Lookup::BY_RDEV);
        let after = (
            super::scan_path_users_of(&[pid], dirs, &[me]).expect("scan"),
            super::super::process_holds_rdev(pid, rdev),
        );
        let old = super::super::hook::reading_dead_as_unknown(|| {
            (
                super::scan_path_users_of(&[pid], dirs, &[me]).expect("scan"),
                super::super::process_holds_rdev(pid, rdev),
            )
        });
        let _ = child.kill();
        let _ = child.wait();
        drop(master);

        assert_eq!(
            before.0.users,
            [super::PathUse {
                pid,
                how: "fd",
                path: slave.clone()
            }],
            "before the revoke, a user through the slave"
        );
        assert_eq!(before.1, Some(true), "before the revoke, holding it");
        assert_eq!(revoked, 0, "revoke: {revoke_err}");
        let fds = fds.expect("its table, read");
        assert!(
            fds.contains(&(3, super::super::ffi::PROX_FDTYPE_VNODE)),
            "the descriptor is still open: {fds:?}"
        );
        assert_eq!(
            read,
            Err(super::super::Failed::Dead),
            "its read answers ENOENT"
        );
        assert_eq!(
            (after.0.scanned, after.0.users, after.0.unreadable),
            (1, Vec::new(), Vec::new()),
            "read, and no user"
        );
        assert_eq!(after.1, Some(false), "it holds no device: known");
        assert_eq!(
            old.0.unreadable.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            [pid],
            "the old reading: unreadable"
        );
        assert_eq!(old.1, None, "the old reading: unknown");
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_dead_working_directory_is_no_working_directory() {
        use super::super::hook::{reading_cwd_dead, reading_dead_as_unknown};
        use crate::spawnfd::Stdio;
        let me = std::process::id();
        let base = fresh("deadcwd");
        let (mnt, held) = (base.join("mnt"), base.join("held"));
        std::fs::create_dir_all(&mnt).expect("the folder standing in for the volume");
        std::fs::create_dir_all(&held).expect("the held file's directory");
        let file = held.join("open.rlib");
        std::fs::write(&file, b"x").expect("a file to hold");
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
            .expect("a child working in the folder");
        let pid = running(&child, "/bin/cat");
        let scan = |dir: &PathBuf| {
            super::scan_path_users_of(&[pid], std::slice::from_ref(dir), &[me]).expect("scan")
        };
        let n = pid as i32;
        let before = scan(&mnt);
        let read = reading_cwd_dead(n, || super::mac::cwd(n));
        let after = reading_cwd_dead(n, || scan(&mnt));
        let through_fd = reading_cwd_dead(n, || scan(&held));
        let old = reading_dead_as_unknown(|| reading_cwd_dead(n, || scan(&mnt)));
        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(
            before.users,
            [super::PathUse {
                pid,
                how: "cwd",
                path: mnt.clone()
            }],
            "a user through its working directory while it reads"
        );
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
            [super::PathUse {
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
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_process_with_no_descriptor_is_read_and_holds_nothing() {
        let dir = fresh("bare");
        let dirs = std::slice::from_ref(&dir);
        let null = {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::metadata("/dev/null").expect("stat").rdev()
        };
        let (away_child, away) = bare_child(std::path::Path::new("/"));
        let fds = super::super::list_fds(away);
        let scan = super::scan_path_users_of(&[away as u32], dirs, &[]).expect("scan");
        let holds = super::super::process_holds_rdev(away as u32, null);
        let (inside_child, inside) = bare_child(&dir);
        let from_inside = super::scan_path_users_of(&[inside as u32], dirs, &[]).expect("scan");
        reap(away_child);
        reap(inside_child);
        assert_eq!(fds, Ok(Vec::new()), "an empty table, read");
        assert_eq!(
            (scan.scanned, scan.users, scan.unreadable),
            (1, Vec::new(), Vec::new()),
            "read, and no user"
        );
        assert_eq!(holds, Some(false), "it holds no device: known, not unknown");
        assert_eq!(
            from_inside.users.iter().map(|u| u.how).collect::<Vec<_>>(),
            ["cwd"]
        );
        let refused = super::scan_path_users_of(&[1], dirs, &[]).expect("scan");
        assert_eq!(
            (refused.scanned, refused.users, refused.unreadable),
            (1, Vec::new(), Vec::new()),
            "launchd's refused table: root's process, outside this user's scan"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_process_that_exited_after_the_listing_uses_nothing() {
        let me = std::process::id();
        let dir = fresh("exited");
        let dirs = std::slice::from_ref(&dir);
        let read = |pid: u32| super::scan_path_users_of(&[pid], dirs, &[me]).expect("scan");
        let gone = |scan: &super::PathScan| scan.users.is_empty() && scan.unreadable.is_empty();

        let mut worker = crate::spawnfd::command("/bin/sleep")
            .arg("120")
            .current_dir(&dir)
            .spawn()
            .expect("a worker");
        let pid = exec_done(&worker);
        let alive = read(pid);
        super::super::tests::kill_unreaped(pid);
        assert!(
            super::super::uid_pids().expect("listing").contains(&pid),
            "the zombie is still listed: a scan meets it"
        );
        let zombie = read(pid);
        let _ = worker.wait();
        let reaped = read(pid);
        assert_eq!(
            alive.users.iter().map(|u| u.how).collect::<Vec<_>>(),
            ["cwd"]
        );
        assert!(gone(&zombie), "a zombie: {zombie:?}");
        assert!(gone(&reaped), "reaped: {reaped:?}");

        let file = dir.join("held.rlib");
        std::fs::write(&file, b"x").expect("rlib");
        let mut holder = crate::spawnfd::command("/bin/sh")
            .arg("-c")
            .arg("exec 3<\"$1\"; exec /bin/sleep 120")
            .arg("sh")
            .arg(&file)
            .current_dir("/")
            .spawn()
            .expect("a holder");
        let pid = exec_done(&holder);
        let holding = read(pid);
        let listed = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let seen = std::rc::Rc::clone(&listed);
        super::super::hook::after_next_listing(move |at, fds| {
            assert_eq!(at, pid as i32);
            seen.set(fds.len());
            super::super::tests::kill_unreaped(pid);
        });
        let mid_read = read(pid);
        let _ = holder.wait();
        assert_eq!(
            holding.users.iter().map(|u| u.how).collect::<Vec<_>>(),
            ["fd"]
        );
        assert!(
            listed.get() > 0,
            "its descriptors were listed, then read after its exit"
        );
        assert!(gone(&mid_read), "killed mid-read: {mid_read:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_data_volume_path_matches_its_firmlinked_form() {
        let dirs = [PathBuf::from("/Users//x/proj")];
        let at = |p: &str| placed(std::path::Path::new(p), &dirs);
        assert_eq!(
            at("/System/Volumes/Data/Users//x/proj/target"),
            Some(PathBuf::from("/Users//x/proj/target"))
        );
        assert_eq!(at("/Users//x/proj"), Some(PathBuf::from("/Users//x/proj")));
        assert_eq!(at("/Users//x/project"), None);
        assert_eq!(at("/System/Volumes/Data/Users//x/project"), None);
    }
}

#[cfg(all(test, windows))]
mod win_tests {
    use super::win::{
        Namer, disk_device_itself, helper_errors_quiet, stuck_for, win32_normalized,
        with_spelling_stuck,
    };
    use super::{Exe, PathBuf, exe_of, scan_path_users, scan_path_users_of};
    use aterm_sysprobe::volumes::{Mount, NETWORK, Table};
    use std::path::Path;
    use std::time::{Duration, Instant};

    const HANG: Duration = Duration::from_secs(60);

    fn scan_of(pids: &[u32], dirs: &[PathBuf], skip: &[u32]) -> std::io::Result<super::PathScan> {
        super::scan_path_users_of_within(pids, dirs, skip, HANG)
    }

    fn two_drives() -> Table {
        let volume = |on: &str, n: u32, guid: &str| Mount {
            on: PathBuf::from(on),
            from: format!(r"\Device\HarddiskVolume{n}"),
            fstype: String::new(),
            local: true,
            automounted: false,
            removable: false,
            read_only: false,
            container: Some(format!("Volume{{{guid}}}")),
        };
        Table {
            mounts: vec![
                Mount {
                    on: PathBuf::from(NETWORK),
                    from: "UNC".to_owned(),
                    fstype: String::new(),
                    local: false,
                    automounted: false,
                    removable: false,
                    read_only: false,
                    container: None,
                },
                volume(r"C:\", 3, "6140fc3b-044f-43d7-b060-cb98170c7a63"),
                volume(r"D:\", 4, "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d"),
            ],
            complete: true,
            firmlinks: Vec::new(),
        }
    }

    fn placed(path: &Path, dirs: &[PathBuf]) -> Option<bool> {
        super::win::placed(path, dirs, &two_drives(), &mut Namer::within(HANG)).under
    }

    fn placed_here(path: &Path, dirs: &[PathBuf]) -> Option<bool> {
        placement_here(path, dirs).under
    }

    fn placement_here(path: &Path, dirs: &[PathBuf]) -> super::win::Placed {
        let table = aterm_sysprobe::volumes::table();
        super::win::placed(path, dirs, &table, &mut Namer::within(HANG))
    }

    const PARK_ENV: &str = "ATERM_UDS_WIN_PARK";
    const PARK_HOLD: &str = "ATERM_UDS_WIN_HOLD";
    const PARK_LOAD: &str = "ATERM_UDS_WIN_LOAD";
    const PARK_WATCH: &str = "ATERM_UDS_WIN_WATCH";
    const PARK_READY: &str = "ATERM_UDS_WIN_READY";
    const PARK_DEVICE: &str = "ATERM_UDS_WIN_DEVICE";
    const PARK_DENY: &str = "ATERM_UDS_WIN_DENY";

    #[derive(Default)]
    struct Park<'a> {
        hold: Option<&'a Path>,
        load: Option<&'a Path>,
        watch: Option<&'a Path>,
        ready: Option<&'a Path>,
        device: Option<&'a Path>,
        deny: bool,
    }

    #[test]
    fn park_mode_sleeps() {
        use std::os::windows::ffi::OsStrExt as _;
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::os::windows::io::AsRawHandle as _;
        const DONT_RESOLVE_DLL_REFERENCES: u32 = 0x0000_0001;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const NOTIFY_ALL: u32 = 0x0000_001F;
        if std::env::var_os(PARK_ENV).is_none() {
            return;
        }
        if std::env::var_os(PARK_DENY).is_some() {
            const SDDL_REVISION: u32 = 1;
            const DACL: u32 = 0x0000_0004;
            let sddl: Vec<u16> = "D:(D;;0x50;;;WD)(A;;0x101000;;;WD)"
                .encode_utf16()
                .chain([0])
                .collect();
            let mut sd: *mut core::ffi::c_void = std::ptr::null_mut();
            // SAFETY: `sddl` is NUL-terminated and `sd` receives a system-allocated security descriptor.
            let made = unsafe {
                crate::win::ffi::ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION,
                    &mut sd,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(
                made,
                0,
                "build the DACL: {}",
                std::io::Error::last_os_error()
            );
            // SAFETY: `GetCurrentProcess()` is the current-process pseudo-handle and `sd` is valid.
            let set = unsafe {
                crate::win::ffi::SetKernelObjectSecurity(
                    crate::win::ffi::GetCurrentProcess(),
                    DACL,
                    sd,
                )
            };
            assert_ne!(set, 0, "set the DACL: {}", std::io::Error::last_os_error());
        }
        let _held = std::env::var_os(PARK_HOLD).map(|path| {
            std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?} to hold: {e}"))
        });
        let _device = std::env::var_os(PARK_DEVICE).map(|path| {
            std::fs::OpenOptions::new()
                .access_mode(0)
                .open(&path)
                .unwrap_or_else(|e| panic!("open {path:?} with no access: {e}"))
        });
        if let Some(module) = std::env::var_os(PARK_LOAD) {
            let wide: Vec<u16> = module.encode_wide().chain([0]).collect();
            // SAFETY: `wide` is a NUL-terminated path loaded as a data file for the process lifetime.
            let mapped = unsafe {
                crate::win::ffi::LoadLibraryExW(wide.as_ptr(), 0, DONT_RESOLVE_DLL_REFERENCES)
            };
            assert_ne!(
                mapped,
                0,
                "map {module:?}: {}",
                std::io::Error::last_os_error()
            );
        }
        let watched = std::env::var_os(PARK_WATCH).map(|dir| {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(&dir)
                .unwrap_or_else(|e| panic!("open {dir:?} to watch: {e}"))
        });
        if let Some(ready) = std::env::var_os(PARK_READY) {
            std::fs::write(&ready, b"ready").expect("say the child is set up");
        }
        if let Some(dir) = &watched {
            let mut buf = [0u32; 1024];
            let mut got: u32 = 0;
            // SAFETY: `dir` is a synchronous directory handle and `buf` is 4096 aligned writable bytes.
            let _ = unsafe {
                crate::win::ffi::ReadDirectoryChangesW(
                    dir.as_raw_handle() as crate::win::ffi::RawHandle,
                    buf.as_mut_ptr().cast(),
                    4096,
                    1,
                    NOTIFY_ALL,
                    &mut got,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
        }
        std::thread::sleep(Duration::from_secs(120));
    }

    fn fresh(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("aterm-uds-winpaths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        std::fs::canonicalize(&d).expect("canonical")
    }

    fn spawn_parked(cwd: &Path, hold: Option<&Path>) -> Parked {
        spawn_parked_with(
            cwd,
            &Park {
                hold,
                ..Park::default()
            },
        )
    }

    struct Parked(crate::spawnfd::Child);
    impl std::ops::Deref for Parked {
        type Target = crate::spawnfd::Child;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }
    impl std::ops::DerefMut for Parked {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }
    impl Drop for Parked {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn spawn_parked_with(cwd: &Path, park: &Park<'_>) -> Parked {
        const NAME: &str = "holders::paths::win_tests::park_mode_sleeps";
        let exe = std::env::current_exe().expect("the test binary");
        let mut cmd = crate::spawnfd::command(exe);
        cmd.args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(PARK_ENV, "1")
            .current_dir(cwd)
            .stdin(crate::spawnfd::Stdio::null())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null());
        for (var, path) in [
            (PARK_HOLD, park.hold),
            (PARK_LOAD, park.load),
            (PARK_WATCH, park.watch),
            (PARK_READY, park.ready),
            (PARK_DEVICE, park.device),
        ] {
            if let Some(path) = path {
                cmd.env(var, path);
            }
        }
        if park.deny {
            cmd.env(PARK_DENY, "1");
        }
        Parked(cmd.spawn().expect("spawn a parked child"))
    }

    fn wait_ready(child: &mut crate::spawnfd::Child, ready: &Path) {
        let start = Instant::now();
        while !ready.exists() {
            assert!(
                child.try_wait().expect("try_wait").is_none(),
                "the child exited before it was set up"
            );
            assert!(start.elapsed() < HANG, "the child never said it was set up");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn until(
        dirs: &[PathBuf],
        child: &mut crate::spawnfd::Child,
        want: impl Fn(Option<&str>) -> bool,
    ) -> Option<String> {
        let pid = child.id();
        let start = Instant::now();
        loop {
            let scan = scan_of(&[pid], dirs, &[std::process::id()]).expect("scan");
            let how = scan
                .users
                .iter()
                .find(|u| u.pid == pid)
                .map(|u| u.how.to_owned());
            if want(how.as_deref()) {
                return how;
            }
            assert!(
                child.try_wait().expect("try_wait").is_none(),
                "the child exited before it was read"
            );
            assert!(start.elapsed() < HANG, "never read as wanted");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn for_cmd(path: &Path) -> String {
        let s = path.to_str().expect("a test path is UTF-8");
        s.strip_prefix(r"\\?\").unwrap_or(s).to_owned()
    }

    #[test]
    fn a_child_working_under_the_directory_uses_it() {
        let dir = fresh("cwd");
        let deep = dir.join("target").join("debug").join("deps");
        std::fs::create_dir_all(&deep).expect("deps");
        let mut worker = spawn_parked(&deep, None);
        let how = until(std::slice::from_ref(&dir), &mut worker, |h| h.is_some());

        let empty = fresh("nobody");
        let me = std::process::id();
        let scan = scan_of(&[worker.id(), me], std::slice::from_ref(&empty), &[me]).expect("scan");
        let _ = worker.kill();
        let _ = worker.wait();
        assert_eq!(how.as_deref(), Some("cwd"));
        assert!(scan.users.is_empty(), "{:?}", scan.users);
        assert!(scan.unreadable.is_empty(), "{:?}", scan.unreadable);
        assert_eq!(
            scan.scanned, 1,
            "the child alone is read; this process is skipped"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn a_child_started_in_a_short_spelling_uses_its_folder_and_no_other() {
        let dir = fresh("short");
        let folder = dir.join("a folder with a long name");
        std::fs::create_dir(&folder).expect("the long-named folder");
        let short = aterm_sysprobe::short_spelling(&folder).unwrap_or_else(|| {
            panic!(
                "PREMISE: the volume generates 8.3 short names; none for {}",
                folder.display()
            )
        });
        let other = fresh("short-other");
        let ready = dir.join("short.ready");
        let park = Park {
            ready: Some(&ready),
            ..Park::default()
        };
        let mut child = spawn_parked_with(Path::new(&for_cmd(&short)), &park);
        wait_ready(&mut child, &ready);
        let pid = child.id();
        let reported = aterm_sysprobe::process_cwd(pid).expect("its working directory");
        let table = aterm_sysprobe::volumes::table();
        assert!(
            aterm_sysprobe::paths::short_spelled(&reported).is_some(),
            "{reported:?} is short"
        );
        assert_eq!(table.is_under(&reported, &folder), None, "{reported:?}");

        let mine = scan_of(&[pid], std::slice::from_ref(&folder), &[]).expect("scan");
        let how = mine.users.iter().find(|u| u.pid == pid).map(|u| u.how);
        assert_eq!(how, Some("cwd"), "{:?}", mine.users);
        let theirs = scan_of(&[pid], std::slice::from_ref(&other), &[]).expect("scan");
        assert!(theirs.users.is_empty(), "{:?}", theirs.users);
        assert!(theirs.unreadable.is_empty(), "{:?}", theirs.unreadable);
        let both = scan_of(&[pid], &[folder.clone(), other.clone()], &[]).expect("scan");
        drop(child);
        let found = both.users.iter().find(|u| u.pid == pid).expect("a user");
        assert_eq!(found.how, "cwd", "{found:?}");
        assert_eq!(
            table.is_under(&found.path, &folder),
            Some(true),
            "{found:?}"
        );
        assert_eq!(
            table.is_under(&found.path, &other),
            Some(false),
            "{found:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn a_child_working_through_a_short_spelled_junction_uses_the_directory_it_is_in() {
        let cand = fresh("short-junction");
        let target = fresh("short-junction-target");
        std::fs::create_dir(target.join("sub")).expect("the target's sub");
        let link = cand.join("a junction with a long name");
        junction(&target, &link);
        let short = aterm_sysprobe::short_spelling(&link.join("sub")).unwrap_or_else(|| {
            panic!(
                "PREMISE: the volume generates 8.3 short names; none for {}",
                link.display()
            )
        });
        let ready = target.join("junction.ready");
        let park = Park {
            ready: Some(&ready),
            ..Park::default()
        };
        let mut child = spawn_parked_with(Path::new(&for_cmd(&short)), &park);
        wait_ready(&mut child, &ready);
        let pid = child.id();
        let reported = aterm_sysprobe::process_cwd(pid).expect("its working directory");
        assert!(
            aterm_sysprobe::paths::short_spelled(&reported).is_some(),
            "{reported:?} is short"
        );

        let mine = scan_of(&[pid], std::slice::from_ref(&cand), &[]).expect("scan");
        let theirs = scan_of(&[pid], std::slice::from_ref(&target), &[]).expect("scan");
        drop(child);
        let how = |scan: &super::PathScan| scan.users.iter().find(|u| u.pid == pid).map(|u| u.how);
        assert_eq!(how(&mine), Some("cwd"), "{mine:?}");
        assert_eq!(how(&theirs), Some("fd"), "{theirs:?}");
        std::fs::remove_dir(&link).expect("the junction, as itself");
        let _ = std::fs::remove_dir_all(&cand);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn a_short_spelling_is_spelled_long_on_the_naming_helper() {
        let dir = fresh("short-helper");
        let folder = dir.join("a folder with a long name");
        std::fs::create_dir(&folder).expect("the long-named folder");
        let short = aterm_sysprobe::short_spelling(&folder).unwrap_or_else(|| {
            panic!(
                "PREMISE: the volume generates 8.3 short names; none for {}",
                folder.display()
            )
        });
        let path = short.join("not-made-yet");
        let normalized = win32_normalized(&path);
        let through = aterm_sysprobe::paths::short_spelled(&normalized)
            .expect("short-spelled")
            .through;
        let dirs = [folder.clone()];
        let held = with_spelling_stuck(&through, || placed_here(&path, &dirs));
        assert_eq!(held, None, "a stuck expansion holds {path:?}");
        let spelled = placement_here(&path, &dirs);
        assert_eq!(spelled.under, Some(true), "{path:?}");
        let key = aterm_sysprobe::paths::windows_key;
        assert_eq!(
            key(&spelled.at),
            key(&folder.join("not-made-yet")),
            "{:?}",
            spelled.at
        );
        assert!(key(&spelled.at).is_some(), "{:?}", spelled.at);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_process_unreadable_once_is_read_again_and_held_only_if_it_fails_again() {
        use super::win::Read;
        let at = |pid: u32| super::PathUse {
            pid,
            how: "cwd",
            path: PathBuf::from(format!(r"C:\d\{pid}")),
        };
        let exe = |pid: u32| Some(PathBuf::from(format!(r"C:\bin\p{pid}.exe")));
        let mut asked: Vec<Vec<u32>> = Vec::new();
        let scan = super::settled(&[1, 2, 3, 4, 5, 6, 7], &[7], |pids| {
            let n = asked.len();
            asked.push(pids.to_vec());
            let read = |pid| match (n, pid) {
                (0, 1) => Read::User(at(1)),
                (0, 2) => Read::None,
                (0, _) => Read::Unreadable(exe(pid)),
                (1, 3) => Read::None,
                (1, 4) => Read::Exited,
                (1, 5) => Read::User(at(5)),
                (1, 6) => Read::Unreadable(None),
                _ => panic!("pass {n} read {pid}"),
            };
            pids.iter().map(|&pid| (pid, read(pid))).collect()
        });
        assert_eq!(asked, [vec![1, 2, 3, 4, 5, 6], vec![3, 4, 5, 6]]);
        assert_eq!(scan.users, [at(1), at(5)]);
        assert_eq!(scan.unreadable, [(6, exe(6))]);
        assert_eq!(scan.scanned, 6);

        let mut passes = 0;
        let clean = super::settled(&[1, 2], &[], |pids| {
            passes += 1;
            pids.iter().map(|&pid| (pid, Read::None)).collect()
        });
        assert_eq!((passes, clean.unreadable.len(), clean.scanned), (1, 0, 2));
    }

    #[test]
    fn a_process_that_refuses_the_open_is_held_without_a_second_read() {
        use super::win::Read;
        let exe = |pid: u32| Some(PathBuf::from(format!(r"C:\Windows\System32\p{pid}.exe")));
        let mut asked: Vec<Vec<u32>> = Vec::new();
        let scan = super::settled(&[1, 2, 3], &[], |pids| {
            asked.push(pids.to_vec());
            let read = |pid| match pid {
                1 => Read::None,
                _ => Read::Refused(exe(pid)),
            };
            pids.iter().map(|&pid| (pid, read(pid))).collect()
        });
        assert_eq!(asked, [vec![1, 2, 3]], "refusals alone make one pass");
        assert_eq!(scan.unreadable, [(2, exe(2)), (3, exe(3))]);
        assert_eq!(scan.scanned, 3);

        let mut asked: Vec<Vec<u32>> = Vec::new();
        let scan = super::settled(&[1, 2, 3], &[], |pids| {
            let n = asked.len();
            asked.push(pids.to_vec());
            let read = |pid| match (n, pid) {
                (0, 1) => Read::Unreadable(exe(1)),
                (0, 2) => Read::Refused(None),
                (0, 3) | (1, 1) => Read::None,
                _ => panic!("pass {n} read {pid}"),
            };
            pids.iter().map(|&pid| (pid, read(pid))).collect()
        });
        assert_eq!(asked, [vec![1, 2, 3], vec![1]], "the failed read alone");
        assert_eq!(scan.unreadable, [(2, None)]);
        assert!(scan.users.is_empty(), "{:?}", scan.users);
    }

    #[test]
    fn a_handle_closed_since_the_snapshot_holds_nothing() {
        use super::win::Read;
        let kind = |read: &Read| match read {
            Read::User(u) => format!("user {}", u.how),
            Read::None => "none".to_owned(),
            Read::Exited => "exited".to_owned(),
            Read::Unreadable(exe) => format!("unreadable {exe:?}"),
            Read::Refused(exe) => format!("refused {exe:?}"),
        };
        let dir = fresh("closed-handle");
        let elsewhere = fresh("closed-handle-elsewhere");
        let ready = dir.join("closed.ready");
        let park = Park {
            ready: Some(&ready),
            ..Park::default()
        };
        let mut child = spawn_parked_with(&dir, &park);
        wait_ready(&mut child, &ready);
        let pid = child.id();
        let table = aterm_sysprobe::volumes::table();
        let mut handles = super::win::file_handles()
            .and_then(|mut all| all.remove(&pid))
            .expect("the child's handles in a system snapshot");
        handles.push(0x00FF_FFFC);
        let mut namer = Namer::within(HANG);
        let mut read = |dirs: &[PathBuf]| {
            kind(&super::win::uses(
                pid,
                dirs,
                &table,
                Some(&handles),
                &mut namer,
            ))
        };
        let free = read(std::slice::from_ref(&elsewhere));
        let used = read(std::slice::from_ref(&dir));
        drop(child);
        assert_eq!(free, "none");
        assert_eq!(used, "user cwd");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn a_child_holding_a_file_under_the_directory_uses_it() {
        let dir = fresh("fd");
        let deep = dir.join("target").join("debug").join("deps");
        std::fs::create_dir_all(&deep).expect("deps");
        let file = deep.join("build-output.log");
        std::fs::write(&file, b"x").expect("a file to hold");
        let mut holder = spawn_parked(&std::env::temp_dir(), Some(&file));
        let how = until(std::slice::from_ref(&dir), &mut holder, |h| h == Some("fd"));
        let _ = holder.kill();
        let _ = holder.wait();
        assert_eq!(how.as_deref(), Some("fd"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_held_file_whose_path_cannot_be_placed_keeps_its_live_holder() {
        use std::os::windows::ffi::OsStringExt as _;
        let dir = fresh("unplaced");
        let deep = dir.join("target").join("debug");
        std::fs::create_dir_all(&deep).expect("debug");
        let name: Vec<u16> = [u16::from(b'o'), 0xD800]
            .into_iter()
            .chain(".log".encode_utf16())
            .collect();
        let unplaceable = deep.join(std::ffi::OsString::from_wide(&name));
        std::fs::write(&unplaceable, b"x").expect("a lone-surrogate name is a legal NTFS name");
        let elsewhere = fresh("unplaced-control");
        let plain = elsewhere.join("plain.log");
        std::fs::write(&plain, b"x").expect("a plain file");

        let read = |hold: &Path, tag: &str| {
            let ready = elsewhere.join(format!("{tag}.ready"));
            let park = Park {
                hold: Some(hold),
                ready: Some(&ready),
                ..Park::default()
            };
            let mut child = spawn_parked_with(&std::env::temp_dir(), &park);
            wait_ready(&mut child, &ready);
            let pid = child.id();
            let scan =
                scan_of(&[pid], std::slice::from_ref(&dir), &[std::process::id()]).expect("scan");
            let alive = child.try_wait().expect("try_wait").is_none();
            let _ = child.kill();
            let _ = child.wait();
            assert!(alive, "the child must still run when it is read");
            (
                scan.users.iter().any(|u| u.pid == pid),
                scan.unreadable.iter().find(|(p, _)| *p == pid).cloned(),
            )
        };

        let (user, unreadable) = read(&unplaceable, "unplaced");
        assert!(
            user || unreadable.is_some(),
            "a live holder of an unplaceable file under the directory read as a non-user"
        );
        if let Some((_, image)) = unreadable {
            assert!(image.is_some(), "a held process is named by its image");
        }
        let (user, unreadable) = read(&plain, "control");
        assert!(!user, "the control holds nothing under the directory");
        assert_eq!(unreadable, None, "the control child must read cleanly");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn a_module_mapped_from_under_the_directory_is_a_user() {
        let dir = fresh("module");
        let deps = dir.join("target").join("debug").join("deps");
        std::fs::create_dir_all(&deps).expect("deps");
        let system = std::env::var_os("SystemRoot").expect("SystemRoot");
        let module = deps.join("probe_macro.dll");
        std::fs::copy(
            Path::new(&system).join("System32").join("version.dll"),
            &module,
        )
        .expect("copy a system DLL under the directory");
        let ready = dir.join("module.ready");
        let park = Park {
            load: Some(&module),
            ready: Some(&ready),
            ..Park::default()
        };
        let mut child = spawn_parked_with(&std::env::temp_dir(), &park);
        wait_ready(&mut child, &ready);
        let pid = child.id();
        let scan =
            scan_of(&[pid], std::slice::from_ref(&deps), &[std::process::id()]).expect("scan");
        let _ = child.kill();
        let _ = child.wait();
        let found = scan.users.iter().find(|u| u.pid == pid);
        assert_eq!(found.map(|u| u.how), Some("exe"), "{scan:?}");
        let name = found
            .and_then(|u| u.path.file_name())
            .and_then(|n| n.to_str());
        assert_eq!(
            name.map(str::to_ascii_lowercase).as_deref(),
            Some("probe_macro.dll"),
            "the module, not the image, is the witness"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn junction(target: &Path, link: &Path) {
        let status = crate::spawnfd::command("cmd")
            .args(["/C", "mklink", "/J", &for_cmd(link), &for_cmd(target)])
            .stdin(crate::spawnfd::Stdio::null())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .status()
            .expect("run mklink");
        assert!(status.success(), "mklink /J {link:?} {target:?}");
    }

    #[test]
    fn a_module_mapped_through_a_junction_from_under_the_directory_is_a_user() {
        let dir = fresh("module-link");
        let real = dir.join("real");
        let deps = real.join("target").join("debug").join("deps");
        std::fs::create_dir_all(&deps).expect("deps");
        let system = std::env::var_os("SystemRoot").expect("SystemRoot");
        std::fs::copy(
            Path::new(&system).join("System32").join("version.dll"),
            deps.join("probe_linked.dll"),
        )
        .expect("copy a system DLL under the directory");
        let link = dir.join("link");
        junction(&real, &link);
        let through = Path::new(&for_cmd(&link))
            .join("target")
            .join("debug")
            .join("deps")
            .join("probe_linked.dll");
        let target = std::fs::canonicalize(real.join("target")).expect("canonical");
        assert_eq!(
            placed_here(&through, std::slice::from_ref(&target)),
            Some(false),
            "the loader's spelling alone places outside the directory"
        );
        let elsewhere = fresh("module-link-control");
        let ready = elsewhere.join("module.ready");
        let park = Park {
            load: Some(&through),
            ready: Some(&ready),
            ..Park::default()
        };
        let mut child = spawn_parked_with(&std::env::temp_dir(), &park);
        wait_ready(&mut child, &ready);
        let pid = child.id();
        let scan =
            scan_of(&[pid], std::slice::from_ref(&target), &[std::process::id()]).expect("scan");
        let control = scan_of(
            &[pid],
            std::slice::from_ref(&elsewhere),
            &[std::process::id()],
        )
        .expect("scan");
        let alive = child.try_wait().expect("try_wait").is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert!(alive, "the child must still run when it is read");
        let found = scan.users.iter().find(|u| u.pid == pid);
        assert_eq!(found.map(|u| u.how), Some("exe"), "{scan:?}");
        let name = found
            .and_then(|u| u.path.file_name())
            .and_then(|n| n.to_str());
        assert_eq!(
            name.map(str::to_ascii_lowercase).as_deref(),
            Some("probe_linked.dll"),
            "the module, not the image, is the witness"
        );
        assert!(control.users.is_empty(), "{control:?}");
        assert!(
            control.unreadable.is_empty(),
            "every mapped module of a plain child is placed: {control:?}"
        );
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn a_mapped_files_kernel_name_is_spelled_through_its_volumes_mount_point() {
        use super::win::on_volume;
        let mut table = two_drives();
        table.mounts.push(Mount {
            on: PathBuf::from(r"C:\mnt\e"),
            from: r"\Device\HarddiskVolume5".to_owned(),
            fstype: String::new(),
            local: true,
            automounted: false,
            removable: false,
            read_only: false,
            container: Some("Volume{11111111-2222-4333-8444-555555555555}".to_owned()),
        });
        table.mounts.push(Mount {
            on: PathBuf::from(r"S:\"),
            from: r"\Device\HarddiskVolume6".to_owned(),
            fstype: aterm_sysprobe::volumes::SUBST.to_owned(),
            local: true,
            automounted: false,
            removable: false,
            read_only: false,
            container: None,
        });
        let on = |kernel: &str| on_volume(Path::new(kernel), &table);
        assert_eq!(
            on(r"\Device\HarddiskVolume3\real\target\debug\deps\x.dll"),
            Some(PathBuf::from(r"\\?\C:\real\target\debug\deps\x.dll"))
        );
        assert_eq!(
            on(r"\device\harddiskvolume4\a"),
            Some(PathBuf::from(r"\\?\D:\a"))
        );
        assert_eq!(
            on(r"\Device\HarddiskVolume5\x\y.dll"),
            Some(PathBuf::from(r"\\?\C:\mnt\e\x\y.dll"))
        );
        assert_eq!(
            placed(
                &on(r"\Device\HarddiskVolume3\Real\Target\x.dll").expect("spelled"),
                &[PathBuf::from(r"\\?\C:\real\target")]
            ),
            Some(true)
        );
        for kernel in [
            r"\Device\Mup\srv\share\x.dll",
            r"\Device\HarddiskVolume9\x.dll",
            r"\Device\HarddiskVolume6\x.dll",
            r"C:\real\x.dll",
            r"\Device\",
        ] {
            assert_eq!(on(kernel), None, "{kernel}");
        }
    }

    #[test]
    fn a_running_image_under_the_directory_is_a_user() {
        let dir = fresh("image");
        let comspec = std::env::var_os("ComSpec").expect("ComSpec");
        let runner = dir.join("runner.exe");
        std::fs::copy(&comspec, &runner).expect("copy cmd.exe into the directory");
        let mut child = crate::spawnfd::command(&runner)
            .arg("/k")
            .current_dir(std::env::temp_dir())
            .stdin(crate::spawnfd::Stdio::piped())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("run the copied image");
        let how = until(std::slice::from_ref(&dir), &mut child, |h| h.is_some());
        drop(child.stdin.take());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(how.as_deref(), Some("exe"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_git_bash_child_working_under_the_directory_uses_it() {
        let sleep = Path::new(r"C:\Program Files\Git\usr\bin\sleep.exe");
        if !sleep.exists() {
            println!(
                "SKIP holders::paths::win_tests::\
                 a_git_bash_child_working_under_the_directory_uses_it: \
                 Git for Windows is not installed ({} absent)",
                sleep.display()
            );
            return;
        }
        let dir = fresh("msys");
        let deep = dir.join("deep");
        std::fs::create_dir_all(&deep).expect("deep");
        let mut child = crate::spawnfd::command(sleep)
            .arg("120")
            .current_dir(&deep)
            .stdin(crate::spawnfd::Stdio::null())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn Git Bash sleep");
        let how = until(std::slice::from_ref(&dir), &mut child, |h| h == Some("cwd"));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(how.as_deref(), Some("cwd"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_32_bit_child_that_moved_under_the_directory_uses_it() {
        use std::io::Write as _;
        let system = std::env::var_os("SystemRoot").expect("SystemRoot");
        let cmd32 = Path::new(&system).join("SysWOW64").join("cmd.exe");
        if !cmd32.exists() {
            println!(
                "SKIP holders::paths::win_tests::\
                 a_32_bit_child_that_moved_under_the_directory_uses_it: \
                 no 32-bit subsystem ({} absent)",
                cmd32.display()
            );
            return;
        }
        let dir = fresh("wow64");
        let deep = dir.join("target").join("debug");
        std::fs::create_dir_all(&deep).expect("debug");
        let mut child = crate::spawnfd::command(&cmd32)
            .args(["/q", "/k"])
            .current_dir(std::env::temp_dir())
            .stdin(crate::spawnfd::Stdio::piped())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("run the 32-bit cmd");
        let mut stdin = child.stdin.take().expect("its stdin");
        write!(stdin, "cd /d \"{}\"\r\n", for_cmd(&deep)).expect("ask it to change directory");
        stdin.flush().expect("flush");
        let how = until(std::slice::from_ref(&dir), &mut child, |h| h == Some("cwd"));
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(how.as_deref(), Some("cwd"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_child_blocked_in_a_synchronous_watch_is_held_and_the_scan_returns() {
        let dir = fresh("watch-scan");
        let watched = fresh("watched");
        let ready = watched.parent().expect("temp").join(format!(
            "aterm-uds-winpaths-watch-{}.ready",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&ready);
        let park = Park {
            watch: Some(&watched),
            ready: Some(&ready),
            ..Park::default()
        };
        let mut child = spawn_parked_with(&std::env::temp_dir(), &park);
        wait_ready(&mut child, &ready);
        let pid = child.id();
        let held = |child: &mut crate::spawnfd::Child| {
            let (tx, rx) = std::sync::mpsc::channel();
            let dirs = vec![dir.clone()];
            let me = std::process::id();
            std::thread::spawn(move || {
                let _ = tx.send(scan_path_users_of(&[pid], &dirs, &[me]));
            });
            let scan = rx
                .recv_timeout(HANG)
                .expect("the scan returns while a child blocks in a watch")
                .expect("scan");
            assert!(
                child.try_wait().expect("try_wait").is_none(),
                "the child exited before it was read"
            );
            assert!(scan.users.is_empty(), "{:?}", scan.users);
            scan.unreadable.iter().any(|(p, _)| *p == pid)
        };
        let start = Instant::now();
        while !held(&mut child) {
            assert!(
                start.elapsed() < HANG,
                "the blocked watch was never read as held"
            );
        }
        assert!(
            stuck_for(pid) > 0,
            "the query that did not answer is remembered"
        );
        assert!(
            held(&mut child),
            "the next scan holds it without asking again"
        );

        let _ = child.kill();
        let _ = child.wait();
        let start = Instant::now();
        while stuck_for(pid) > 0 {
            assert!(
                start.elapsed() < HANG,
                "the abandoned helper never returned once the child ended"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_file(&ready);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn exe_of_names_a_live_image_and_an_exited_one_as_exited() {
        let mut child = spawn_parked(&std::env::temp_dir(), None);
        let pid = child.id();
        let own = std::fs::canonicalize(std::env::current_exe().expect("the test binary"))
            .expect("canonical");
        let named = exe_of(pid);
        let _ = child.kill();
        let _ = child.wait();
        match named {
            Exe::At(image) => assert_eq!(placed_here(&image, &[own]), Some(true), "{image:?}"),
            other => panic!("a live child's image is named, not {other:?}"),
        }
        assert_eq!(exe_of(pid), Exe::Exited, "a waited-for child still held");
    }

    #[test]
    fn verbatim_plain_and_differently_cased_spellings_all_place_under() {
        let dir = vec![PathBuf::from(r"\\?\C:\Users\x\Proj")];
        for spelling in [
            r"\\?\C:\Users\x\Proj\target\debug",
            r"\??\C:\Users\x\Proj\target\debug", // the NT spelling
            r"C:\Users\x\Proj\target\debug",
            r"c:\users\X\PROJ\target\debug",
            r"C:\Users\x\Proj\target\debug\", // a trailing separator
            r"C:\Users\x\Proj",               // the directory itself
            r"C:\Users\x\Proj\",
            r"C:/Users//x/Proj/target", // `/` separates outside a verbatim prefix
        ] {
            assert_eq!(placed(Path::new(spelling), &dir), Some(true), "{spelling}");
        }
        for spelling in [
            r"C:\Users\x\Project\target", // a sibling sharing a prefix
            r"C:\Users\x\ProjOther",
            r"D:\Users\x\Proj\target", // another volume
            r"C:\",
        ] {
            assert_eq!(placed(Path::new(spelling), &dir), Some(false), "{spelling}");
        }
        assert_eq!(placed(Path::new(r"\\?\C:/Users//x/Proj"), &dir), None);
        let root = vec![PathBuf::from(r"\\?\C:\")];
        assert_eq!(placed(Path::new(r"c:\x\y"), &root), Some(true));

        let share = vec![PathBuf::from(r"\\?\UNC\srv\share\Proj")];
        for spelling in [
            r"\\srv\share\Proj\t",
            r"\\?\UNC\SRV\Share\proj",
            r"\\srv\share\Proj",
        ] {
            assert_eq!(
                placed(Path::new(spelling), &share),
                Some(true),
                "{spelling}"
            );
        }
        for spelling in [
            r"\\srv\share\Projx",
            r"\\srv2\share\Proj",
            r"\\srv\other\Proj",
            r"C:\Users\x\Proj",
        ] {
            assert_eq!(placed(Path::new(spelling), &share), None, "{spelling}");
        }
        assert_eq!(placed(Path::new(r"\\srv\share\Proj\t"), &dir), None);
        assert_eq!(placed(Path::new(r"S:\Users\x\Proj\target"), &dir), None);
        let cut = Table {
            complete: false,
            ..two_drives()
        };
        let mut namer = Namer::default();
        let under = super::win::placed(Path::new(r"c:\USERS\x\proj\t"), &dir, &cut, &mut namer);
        assert_eq!(under.under, Some(true));
        let other = super::win::placed(Path::new(r"D:\Users\x\Proj\t"), &dir, &cut, &mut namer);
        assert_eq!(other.under, None);
    }

    #[test]
    fn a_path_outside_a_drive_or_share_root_cannot_be_placed() {
        use std::os::windows::ffi::OsStringExt as _;
        let dir = vec![PathBuf::from(r"\\?\C:\Users\x\Proj")];
        for spelling in [
            r"\\.\C:\Users\x\Proj\t",
            r"\\?\Volume{0a1b2c3d-0000-0000-0000-100000000000}\Users\x\Proj",
            r"\\?\GLOBALROOT\Device\HarddiskVolume3\Users\x\Proj",
            r"\Device\HarddiskVolume3\Users\x\Proj",
            r"Users\x\Proj\t",
            r"C:Users\x\Proj",
            r"\Users\x\Proj",
            r"\\server",
            r"\\?\C:\Users\x\Proj\..\..\y",
            r"\\?\C:\Users\x\.\Proj",
            r"\\?\C:\Users\x\\Proj\t",
            r"C:\Users\x\Proj.\t",
            r"C:\Users\x\Proj \t",
        ] {
            assert_eq!(placed(Path::new(spelling), &dir), None, "{spelling}");
        }
        let empty = fresh("short-none");
        let ghost = empty.join("NOSUCH~1").join("t");
        assert_eq!(
            placed(&ghost, std::slice::from_ref(&empty)),
            None,
            "{ghost:?}"
        );
        let _ = std::fs::remove_dir_all(&empty);
        assert_eq!(
            placed(Path::new(r"\\?\C:\Users\x\Proj.\t"), &dir),
            Some(false)
        );
        let lone: Vec<u16> = r"\\?\C:\Users\x\P".encode_utf16().chain([0xD800]).collect();
        let unkeyable = PathBuf::from(std::ffi::OsString::from_wide(&lone));
        let path = Path::new(r"C:\Users\x\Proj\t");
        assert_eq!(placed(path, std::slice::from_ref(&unkeyable)), None);
        assert_eq!(placed(path, &[unkeyable, dir[0].clone()]), Some(true));
    }

    #[test]
    fn a_win32_path_places_as_windows_normalizes_it() {
        let dir = vec![PathBuf::from(r"\\?\C:\Users\x\Proj")];
        for spelling in [
            r"C:\Users\x\Other\..\Proj\t",
            r"C:\Users\x\.\Proj",
            r"C:\Users\x\\Proj\t",
            r"C:\..\..\Users\x\Proj",
            r"\\?\C:\Users\x\Proj\t",
        ] {
            assert_eq!(placed(Path::new(spelling), &dir), Some(true), "{spelling}");
        }
        for spelling in [r"C:\Users\x\Proj\..\Other", r"C:\Users\x\Proj\t\..\..\.."] {
            assert_eq!(placed(Path::new(spelling), &dir), Some(false), "{spelling}");
        }
        let bash = Path::new(r"C:\Program Files\Git\bin\..\usr\bin\bash.exe");
        let usr = vec![PathBuf::from(r"\\?\C:\Program Files\Git\usr")];
        let bin = vec![PathBuf::from(r"\\?\C:\Program Files\Git\bin")];
        assert_eq!(placed(bash, &usr), Some(true));
        assert_eq!(placed(bash, &bin), Some(false));
        let share = vec![PathBuf::from(r"\\?\UNC\srv\share\Proj")];
        assert_eq!(
            placed(Path::new(r"\\srv\share\x\..\Proj\t"), &share),
            Some(true)
        );
        assert_eq!(
            placed(Path::new(r"\\srv\share\..\Proj"), &share),
            Some(true)
        );
        assert_eq!(
            placed(Path::new(r"\\srv\share\..\other\Proj"), &share),
            None
        );
        for (from, to) in [
            (r"C:\a\.\b\..\c", r"C:\a\c"),
            (r"C:\..", r"C:\"),
            (r"c:/x//y/", r"c:\x\y"),
            (r"\\srv\share\..\..\x", r"\\srv\share\x"),
            (r"\\?\C:\a\..\b", r"\\?\C:\a\..\b"),
            (r"\\.\C:\a\..\b", r"\\.\C:\a\..\b"),
            (r"C:a\..\b", r"C:a\..\b"),
        ] {
            assert_eq!(win32_normalized(Path::new(from)), Path::new(to), "{from}");
        }
    }

    #[test]
    fn the_naming_helper_holds_the_error_boxes_off() {
        assert_eq!(helper_errors_quiet(HANG), Some(true), "the helper's thread");
        assert!(
            !aterm_sysprobe::errors_quiet(),
            "a thread with no guard has them on"
        );
    }

    #[test]
    fn only_a_disk_device_opened_itself_is_no_file() {
        for name in [
            r"\Device\HarddiskVolume3",
            r"\Device\Harddisk0\DR0",
            r"\Device\Harddisk12\Partition2",
            r"\Device\CdRom0",
        ] {
            assert!(disk_device_itself(name), "{name}");
        }
        for name in [
            r"\Device\HarddiskVolume3\Users\x",
            r"\Device\HarddiskVolume3\",
            r"\Device\HarddiskVolumeShadowCopy1",
            r"\Device\HarddiskVolume",
            r"\Device\Harddisk0\DR0\x",
            r"\Device\Harddisk0",
            r"\Device\Mup\srv\share",
            r"\Device\NamedPipe\x",
            r"\Device\Null",
            r"\??\C:",
            "",
        ] {
            assert!(!disk_device_itself(name), "{name}");
        }
    }

    #[test]
    fn a_child_that_refuses_the_reads_is_held_and_named() {
        let dir = fresh("refused");
        let ready = dir.join("refused.ready");
        let park = Park {
            ready: Some(&ready),
            deny: true,
            ..Park::default()
        };
        let mut child = spawn_parked_with(&dir, &park);
        wait_ready(&mut child, &ready);
        let pid = child.id();
        let scan =
            scan_of(&[pid], std::slice::from_ref(&dir), &[std::process::id()]).expect("scan");
        let named = exe_of(pid);
        let alive = child.try_wait().expect("try_wait").is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert!(alive, "the child must still run when it is read");
        let own = std::fs::canonicalize(std::env::current_exe().expect("the test binary"))
            .expect("canonical");
        assert!(scan.users.is_empty(), "{:?}", scan.users);
        match scan.unreadable.iter().find(|(p, _)| *p == pid) {
            Some((_, Some(image))) => {
                assert_eq!(
                    placed_here(image, std::slice::from_ref(&own)),
                    Some(true),
                    "{image:?}"
                );
            }
            other => panic!("a refusing live child is held and named, not {other:?}"),
        }
        match named {
            Exe::At(image) => assert_eq!(placed_here(&image, &[own]), Some(true), "{image:?}"),
            other => panic!("its image is named, not {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_child_holding_a_volume_open_reads_cleanly() {
        let dir = fresh("volume");
        let ready = dir.join("volume.ready");
        let drive = std::env::var("SystemDrive").expect("SystemDrive");
        let volume = PathBuf::from(format!(r"\\.\{drive}"));
        let park = Park {
            device: Some(&volume),
            ready: Some(&ready),
            ..Park::default()
        };
        let mut child = spawn_parked_with(&std::env::temp_dir(), &park);
        wait_ready(&mut child, &ready);
        let pid = child.id();
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).expect("empty");
        let scan =
            scan_of(&[pid], std::slice::from_ref(&empty), &[std::process::id()]).expect("scan");
        let alive = child.try_wait().expect("try_wait").is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert!(alive, "the child must still run when it is read");
        assert!(scan.users.is_empty(), "{:?}", scan.users);
        assert!(scan.unreadable.is_empty(), "{:?}", scan.unreadable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn case_folds_one_unit_to_one() {
        let strasse = vec![PathBuf::from(r"\\?\C:\Straße")];
        assert_eq!(placed(Path::new(r"C:\STRAßE\x"), &strasse), Some(true));
        assert_eq!(placed(Path::new(r"c:\straße\x"), &strasse), Some(true));
        assert_eq!(placed(Path::new(r"C:\STRASSE\x"), &strasse), Some(false));
        let umlaut = vec![PathBuf::from(r"\\?\C:\Ärger")];
        assert_eq!(placed(Path::new(r"c:\ärger\x"), &umlaut), Some(true));
        let greek = vec![PathBuf::from("\\\\?\\C:\\\u{1FB3}")];
        assert_eq!(placed(Path::new("C:\\\u{1FBC}\\x"), &greek), Some(true));
        let deseret = vec![PathBuf::from("\\\\?\\C:\\\u{10428}")];
        assert_eq!(placed(Path::new("C:\\\u{10400}\\x"), &deseret), Some(false));
    }

    #[test]
    #[ignore = "a measurement, run by hand"]
    fn the_scan_times_itself_and_names_unreadable_processes() {
        let empty = fresh("measure");
        let start = Instant::now();
        let scan = scan_path_users(std::slice::from_ref(&empty), &[std::process::id()])
            .expect("the full scan");
        let elapsed = start.elapsed();
        println!(
            "MEASURED holders::paths scan: {} same-user processes in {:.1} ms, {} unreadable",
            scan.scanned,
            elapsed.as_secs_f64() * 1000.0,
            scan.unreadable.len()
        );
        for (pid, exe) in &scan.unreadable {
            println!(
                "  unreadable pid {pid}: {}",
                exe.as_ref()
                    .map_or("image unnamed".to_owned(), |e| e.display().to_string())
            );
        }
        assert!(
            scan.scanned > 0,
            "some process of this account besides this one (skipped, so not counted) is scanned"
        );
        let _ = std::fs::remove_dir_all(&empty);
    }
}
