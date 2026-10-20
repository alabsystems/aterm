// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The path-user scan: which live processes of this uid use a path under one of a set of
//! directories, as their working directory, their executable, or through an open descriptor.
//!
//! The disk watch removes a build directory whole only when its owner is gone
//! (`aterm_agent::harness::disk`). Cargo takes no lock while tests run, and a binary run from
//! `deps/` holds none either, so "gone" is also checked against what is running: a process
//! whose working directory, executable or open file lies under the directory (or the owner's
//! own folder) keeps it.
//!
//! macOS reads it through libproc, as the rdev scan beside it does: per process
//! `proc_pidinfo(PROC_PIDVNODEPATHINFO)` (the working directory), `proc_pidpath` (the
//! executable) and, per vnode descriptor, `proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO)`. Linux
//! reads `/proc/<pid>/cwd`, `exe` and `fd/*`. A process that exited holds nothing (`ESRCH`, or
//! its `/proc` entry gone), whenever it exited; a descriptor closed mid-read, or on a dead
//! vnode (the holder scan's module docs), names no path; a process with no descriptor is read.
//!
//! So is a process whose working directory is a dead vnode: it works in no path, and its
//! executable and descriptors still say what it uses (read as unknown, it would keep every
//! gone owner's build directory, `aterm_agent::harness::disk_discover::unused`). A directory
//! is never revoked (`revoke(2)` takes devices alone), so a working directory dies with its
//! volume's forced unmount. In xnu (xnu-12377.121.6, `bsd/kern/proc_info.c`)
//! `PROC_PIDVNODEPATHINFO` answers `vnode_getwithvid`'s `ENOENT` (reclaimed, or its vid moved)
//! before reading anything else; a later refusal (MAC, same-uid, getattr) leaves the process
//! unreadable, and a getattr `ENOENT` is a directory that no longer exists. A dead root
//! directory answers the same `ENOENT`, which misreads only a superuser's `chroot(2)` on a
//! force-unmounted volume. An `ENOENT` listing a table stays unknown.
//!
//! Linux has no such failure to tell apart: a hung-up terminal keeps its path in
//! `/proc/<pid>/fd/<n>` (`__tty_hangup` swaps only the file's operations), and a deleted
//! target reads as its path and ` (deleted)`. A process whose answers could not be read
//! otherwise is [`PathScan::unreadable`], never a non-user. Paths are compared as the kernel
//! writes them, with the directories given canonical; a kernel path under
//! `/System/Volumes/Data` (the data volume's firmlinked form) is compared without that prefix
//! too. Memory-mapped files that hold no descriptor are not seen.
//!
//! Other platforms answer `Unsupported`.

use std::io;
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
use std::path::Path;
use std::path::PathBuf;

/// One process using a path under one of the directories scanned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathUse {
    /// The process.
    pub pid: u32,
    /// How: `"cwd"`, `"exe"` or `"fd"`.
    pub how: &'static str,
    /// The path, as the kernel wrote it.
    pub path: PathBuf,
}

/// What one scan found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PathScan {
    /// Each process found using a path under one of the directories (the
    /// first such use of each).
    pub users: Vec<PathUse>,
    /// Processes scanned whose working directory, executable or descriptors could not be read
    /// (not because they exited), each with its executable when that much could be read. On
    /// macOS some platform agents (`lsd`, `UserEventAgent`, `routined`, `nsurlsessiond`)
    /// refuse a same-uid reader.
    pub unreadable: Vec<(u32, Option<PathBuf>)>,
    /// How many processes were scanned.
    pub scanned: usize,
}

/// Is the kernel's `path` at or under one of `dirs`?
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn under(path: &Path, dirs: &[PathBuf]) -> bool {
    let data = path.strip_prefix("/System/Volumes/Data").ok();
    dirs.iter().any(|d| {
        path.starts_with(d) || data.is_some_and(|rel| Path::new("/").join(rel).starts_with(d))
    })
}

/// Scan every live process of this uid except `skip` for a working
/// directory, an executable or an open descriptor at or under one of `dirs`
/// (each canonical). A child of a `skip`ped process that still runs its
/// parent's executable — a fork that has not exec'd yet, holding copies of
/// every descriptor its parent had — is skipped with it: what it holds is
/// the parent's. That executable must be this process's own, run
/// untranslated: under a translator every program it runs has the same one
/// (`unexeced_fork`).
///
/// # Errors
///
/// The process listing itself failing; `Unsupported` on a platform with no
/// reader.
#[cfg(target_vendor = "apple")]
// Skip: bottoms out at the libproc FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
pub fn scan_path_users(dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    scan_path_users_of(&super::uid_pids()?, dirs, skip)
}

/// [`scan_path_users`] over the processes `pids` alone, each read as that scan reads it: the
/// full scan is this over every process of the uid, and a test names the processes it started
/// so nothing else the machine runs reaches its verdict. A pid that names no process has
/// exited and uses nothing. Every pid named is read whatever its uid (the uid filters only the
/// full scan's listing), so another uid's process is mostly [`PathScan::unreadable`].
///
/// # Errors
///
/// `Unsupported` on a platform with no reader.
#[cfg(target_vendor = "apple")]
// Skip: bottoms out at the libproc FFI calls.
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
            Err((super::Failed::Closed | super::Failed::Dead | super::Failed::Other, exe)) => {
                scan.unreadable.push((pid, exe));
            }
        }
    }
    Ok(scan)
}

/// Is `pid` a child of one of `parents` still running that parent's
/// executable (a fork that has not exec'd)? An equal executable proves that
/// only when it names a program, not an interpreter that runs others: this
/// process's own, read untranslated ([`own_program`]). Any other proves
/// nothing, and the child is a user, the keep this scan exists for: it is
/// asked before a build directory is deleted.
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

/// The executable that names the program this process runs: the kernel's
/// answer. macOS has no `binfmt_misc` to hand a process to an interpreter, so
/// the kernel names the image it loaded.
#[cfg(target_vendor = "apple")]
fn own_program() -> Option<PathBuf> {
    executable(std::process::id())
}

/// See the macOS twin. On Linux a translated process's executable is its
/// interpreter (`binfmt_misc`): under Rosetta this process, its un-exec'd
/// fork and every other x86_64 program it has exec'd all read as
/// `/mnt/lima-rosetta/rosetta` ([`exe_of`], measured 2026-10-17), so an equal
/// executable cannot tell a fork from a child running another program. This
/// process's executable names its program only when no translator runs it,
/// and either of two reads says when one does: a translator rewrites this
/// process's path read of its own link to the program it translates, and the
/// image the kernel runs is built for another architecture than this code
/// ([`own_image_machine`]). Under Rosetta each alone answers `None` (measured
/// 2026-10-17). `None` too when either cannot be read.
#[cfg(target_os = "linux")]
fn own_program() -> Option<PathBuf> {
    let kernel = executable(std::process::id())?;
    let untranslated =
        std::fs::read_link("/proc/self/exe").ok()? == kernel && own_image_machine()? == ELF_MACHINE;
    untranslated.then_some(kernel)
}

/// The `e_machine` of the ELF image the kernel runs as this process, read
/// through the opened `/proc/<pid>` directory as [`exe_of`] reads its link: a
/// translator rewrites an open of this process's own `/proc/self/exe` to the
/// program it translates (Rosetta does), not one through the directory.
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

/// This code's architecture as an ELF header's `e_machine` names it
/// (`EM_X86_64`, `EM_AARCH64`); `EM_NONE` on any other, where
/// [`own_program`] names nothing and no fork is skipped.
#[cfg(target_os = "linux")]
const ELF_MACHINE: u16 = if cfg!(target_arch = "x86_64") {
    62
} else if cfg!(target_arch = "aarch64") {
    183
} else {
    0
};

/// The parent of `pid` (`proc_pidinfo(PROC_PIDT_SHORTBSDINFO)`'s
/// `pbsi_ppid`).
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
    /// The path it runs, as the kernel holds it.
    At(PathBuf),
    /// It runs, and what it runs has no path to name: unlinked since its exec (a rebuilt
    /// binary, a replaced app), or the read refused.
    Unnamed,
    /// It has exited (`ESRCH`, which a zombie answers too; on Linux, its
    /// `exe` gone): it runs nothing.
    Exited,
}

/// The executable of `pid`, as the kernel holds it, told apart from a process that has exited
/// (one whose executable cannot be named still runs).
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn exe_of(pid: u32) -> Exe {
    match i32::try_from(pid).map(mac::exe) {
        Ok(Ok(Some(path))) => Exe::At(path),
        Ok(Err(super::Failed::Exited)) => Exe::Exited,
        _ => Exe::Unnamed,
    }
}

/// The executable of `pid`, when [`exe_of`] names one.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn executable(pid: u32) -> Option<PathBuf> {
    match exe_of(pid) {
        Exe::At(path) => Some(path),
        Exe::Unnamed | Exe::Exited => None,
    }
}

/// The parent of `pid` (`/proc/<pid>/stat`'s fourth field).
#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// See the macOS twin: `/proc/<pid>/exe`, gone once the process exited
/// (a zombie's too). An executable unlinked since the exec reads as its
/// path and ` (deleted)`.
///
/// It is read through the opened `/proc/<pid>` directory (`readlinkat`), the
/// kernel's own answer for every pid, this process's included. A binary
/// translator rewrites the path read of its own process's link: under Rosetta
/// `readlink("/proc/<own pid>/exe")` names the translated binary, while every
/// other pid's link names the translator (`/mnt/lima-rosetta/rosetta`), and
/// `readlinkat` names the translator for this process too (measured
/// 2026-10-17). [`own_program`] reads the difference.
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
    // The kernel writes at most `PATH_MAX` bytes; a full buffer would be a
    // path cut short, which names nothing.
    let mut buf = [0u8; 4096];
    // SAFETY: `dir` is open for the call, the name is NUL-terminated, and the
    // kernel writes at most `buf.len()` bytes into `buf`.
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
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
#[must_use]
pub fn exe_of(_pid: u32) -> Exe {
    Exe::Unnamed
}

#[cfg(target_vendor = "apple")]
mod mac {
    use super::super::{Failed, clear_errno, ffi, list_fds};
    use super::{PathBuf, PathUse, under};
    use std::os::unix::ffi::OsStrExt as _;

    /// The NUL-terminated path at `buf[at..at + len]`.
    fn path_at(buf: &[u8], at: usize, len: usize) -> Option<PathBuf> {
        let bytes = buf.get(at..at.checked_add(len)?)?;
        let n = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        let p = std::path::Path::new(std::ffi::OsStr::from_bytes(bytes.get(..n)?));
        p.is_absolute().then(|| p.to_path_buf())
    }

    /// The working directory of `pid`, as the kernel answers it:
    /// `Err(Failed::Dead)` when it is a dead vnode ([`uses`] reads that as no
    /// working directory; the module docs say why).
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
            // An executable unlinked since the exec has no path left to report, which is no
            // failure to read the process: its working directory and descriptors still say
            // what it uses.
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if errno == Failed::ENOENT {
                return Ok(None);
            }
            return Err(Failed::of(errno));
        }
        let n = usize::try_from(got).map_err(|_| Failed::Other)?;
        Ok(path_at(&buf, 0, n.min(buf.len())))
    }

    /// The path of `pid`'s vnode descriptor `fd`.
    fn fd_path(pid: i32, fd: i32) -> Result<Option<PathBuf>, Failed> {
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

    /// The first use `pid` makes of a path under `dirs`: `Ok(None)` when
    /// none; `Err(Exited)` when it exited (it uses nothing); any other
    /// failure with the executable, when that much was read.
    pub(super) fn uses(
        pid: u32,
        dirs: &[PathBuf],
    ) -> Result<Option<PathUse>, (Failed, Option<PathBuf>)> {
        let n = i32::try_from(pid).map_err(|_| (Failed::Other, None))?;
        let hit = |how: &'static str, path: Option<PathBuf>| {
            path.filter(|p| under(p, dirs))
                .map(|path| PathUse { pid, how, path })
        };
        let exe = exe(n).map_err(|f| (f, None))?;
        let fail = |f: Failed| (f, exe.clone());
        if let Some(found) = hit("exe", exe.clone()) {
            return Ok(Some(found));
        }
        let cwd = match cwd(n) {
            Ok(path) => path,
            // A dead working directory (its volume force-unmounted) names no
            // path, so it is under none of the directories: the process is
            // read on, through its descriptors.
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
                // Closed since the listing, or on a dead vnode (revoked):
                // it names no path, so none under the directories.
                Err(Failed::Closed | Failed::Dead) => {}
                Err(Failed::Other) => unreadable = true,
            }
        }
        if unreadable {
            Err(fail(Failed::Other))
        } else {
            Ok(None)
        }
    }
}

/// See the macOS twin: every process of this uid, the listing `/proc`'s
/// entries owned by it.
///
/// # Errors
///
/// `/proc` or `/proc/self` could not be read.
#[cfg(target_os = "linux")]
pub fn scan_path_users(dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    scan_path_users_of(&super::uid_pids()?, dirs, skip)
}

/// See the macOS twin: every pid named is read, whatever its uid.
///
/// # Errors
///
/// None: the signature is the macOS twin's.
#[cfg(target_os = "linux")]
pub fn scan_path_users_of(pids: &[u32], dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    let mut scan = PathScan::default();
    for &pid in pids {
        if skip.contains(&pid) {
            continue;
        }
        let proc_dir = PathBuf::from(format!("/proc/{pid}"));
        if std::fs::symlink_metadata(&proc_dir).is_err() {
            continue; // exited
        }
        scan.scanned += 1;
        match linux_uses(&proc_dir, pid, dirs) {
            Ok(Some(found)) if !unexeced_fork(pid, skip) => scan.users.push(found),
            Ok(Some(_)) => {}
            Ok(None) => {}
            Err(()) => scan
                .unreadable
                .push((pid, std::fs::read_link(proc_dir.join("exe")).ok())),
        }
    }
    Ok(scan)
}

/// One `/proc/<pid>`'s first use of a path under `dirs`; `Err` when it could
/// not be read (and has not exited).
#[cfg(target_os = "linux")]
fn linux_uses(proc_dir: &Path, pid: u32, dirs: &[PathBuf]) -> Result<Option<PathUse>, ()> {
    let gone = |e: &io::Error| e.kind() == io::ErrorKind::NotFound;
    for how in ["cwd", "exe"] {
        match std::fs::read_link(proc_dir.join(how)) {
            Ok(p) if under(&p, dirs) => return Ok(Some(PathUse { pid, how, path: p })),
            Ok(_) => {}
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
            Ok(p) if under(&p, dirs) => {
                return Ok(Some(PathUse {
                    pid,
                    how: "fd",
                    path: p,
                }));
            }
            Ok(_) => {}
            Err(e) if gone(&e) => {}
            Err(_) => return Err(()),
        }
    }
    Ok(None)
}

/// See the macOS twin.
///
/// # Errors
///
/// Always `Unsupported`.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub fn scan_path_users(dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    scan_path_users_of(&[], dirs, skip)
}

/// See the macOS twin.
///
/// # Errors
///
/// Always `Unsupported`.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub fn scan_path_users_of(_pids: &[u32], _dirs: &[PathBuf], _skip: &[u32]) -> io::Result<PathScan> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the path-user scan reads libproc (macOS) or /proc (Linux)",
    ))
}

/// A child's pid once the kernel shows it running `exe`.
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
    const HDIUTIL: &str = "/usr/bin/hdiutil";

    /// `hdiutil detach -force <dev>`, waited for.
    #[cfg(target_vendor = "apple")]
    fn detach(dev: &str) -> io::Result<std::process::Output> {
        crate::spawnfd::command(HDIUTIL)
            .args(["detach", "-force", dev])
            .stdin(crate::spawnfd::Stdio::null())
            .output()
    }

    /// A fresh disk image attached at a mount point (no privilege needed, hidden from the
    /// Finder), detached by force when dropped still attached.
    #[cfg(target_vendor = "apple")]
    struct Volume {
        /// Its device (`/dev/diskN`); empty once detached.
        dev: String,
    }

    #[cfg(target_vendor = "apple")]
    impl Volume {
        /// A 2 MB HFS+ image created at `image` and attached at `mnt`; setup
        /// unavailability is an error, never a successful acceptance row. Not `-quiet`: that would hide
        /// the `Operation timed out` the judgment reads.
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

        /// Detach it by force: every vnode on it is reclaimed under whatever still holds one.
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

    /// A child works at the root of a disk image's volume and holds a file elsewhere open;
    /// `hdiutil detach -force` reclaims its working directory's vnode (`ENOENT` from then on).
    /// Scanned for the mount point it is then no user and not unreadable; scanned for the held
    /// file's directory it is a user through that descriptor. Negative control: with dead
    /// vnodes read as unknown, it is unreadable. Run explicitly with the
    /// `dmg_acceptance` bench: mounted-volume access may request macOS consent.
    /// The ordinary injected dead-cwd test retains the deterministic control.
    ///
    /// # Errors
    /// Returns the setup failure when the disk image cannot be created or attached.
    pub fn a_force_detached_volume_is_no_working_directory(base: &Path) -> std::io::Result<()> {
        use crate::spawnfd::Stdio;
        use std::os::unix::fs::MetadataExt as _;
        let me = std::process::id();
        let base = base.to_path_buf();
        let (image, mnt, held) = (base.join("v.dmg"), base.join("mnt"), base.join("held"));
        std::fs::create_dir_all(&mnt).expect("the mount point");
        std::fs::create_dir_all(&held).expect("the held file's directory");
        let file = held.join("open.rlib");
        std::fs::write(&file, b"x").expect("a file to hold");
        let device = |p: &std::path::Path| std::fs::metadata(p).expect("stat").dev();
        let mut volume = Volume::attach(&image, &mnt)?;
        let mounted = device(&mnt) != device(&base);
        // At the volume's root, the file on descriptor 3; `cat` ends when this test's end of
        // its stdin closes, unwinding included.
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
    use super::{PathBuf, scan_path_users, under};
    use std::time::{Duration, Instant};

    const HANG: Duration = Duration::from_secs(60);

    /// A fresh directory of this test's, canonical.
    fn fresh(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aterm-uds-paths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        std::fs::canonicalize(&d).expect("canonical")
    }

    /// Scan until `want` holds for the child `pid`, or the hang detector
    /// fires (a spawned child shows its working directory at once, but its
    /// exec and its opens take a moment).
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

    /// A child working under the directory uses it, and so does one holding a file there
    /// open. Negative control: a directory nobody is in has no user, and this process is
    /// skipped.
    #[test]
    fn a_child_working_or_holding_a_file_under_the_directory_uses_it() {
        let dir = fresh("cwd");
        let deep = dir.join("debug/deps");
        std::fs::create_dir_all(&deep).expect("deps");
        // Every child here is `spawnfd`'s, so it holds no other test thread's pipe.
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
        // `sh` holds the file open on descriptor 3 from another directory.
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

    /// A process whose executable was deleted (`proc_pidpath` answers `ENOENT`) is still read,
    /// and its working directory makes it a user. The copy of `cat` is signed ad hoc, or the
    /// kernel kills it, and deleted only once the child has echoed a line back: a fresh
    /// executable deleted before its launch is done gets its process killed. The child ends at
    /// its EOF.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_process_whose_executable_was_deleted_is_read_through_its_working_directory() {
        use std::io::{BufRead as _, Write as _};
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
        let mut to = catter.stdin.take().expect("its stdin");
        let from = catter.stdout.take().expect("its stdout");
        let (tx, echo) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::BufReader::new(from).read_line(&mut line);
            let _ = tx.send(line);
        });
        to.write_all(b"launched\n").expect("a line to it");
        let echoed = echo.recv_timeout(HANG);
        assert_eq!(echoed.as_deref(), Ok("launched\n"), "it runs");
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

    /// Why a translator leaves the fork test's skip undecided, for its `NOT RUN` line.
    const FORK_UNDER_A_TRANSLATOR: &str = "the kernel names the translator as the executable \
        of this process, of its un-exec'd fork and of every program the translator runs \
        (binfmt_misc), so a fork cannot be told from a child that exec'd another program; the \
        fork is kept as a user instead (asserted), and a real x86_64 CPU decides the skip";

    /// A fork of this process that has not exec'd holds copies of every descriptor this
    /// process had, so it is skipped with it. Negative control: unskipped, it is a user.
    /// Under a translator ([`crate::spawnfd::tests::translator`]) the skip cannot be decided
    /// ([`FORK_UNDER_A_TRANSLATOR`]): the fork must be kept as a user, and the test says the
    /// skip did not run.
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

    /// Present in the environment of the next test's child: hold until stdin closes.
    const PARK_ENV: &str = "ATERM_UDS_PATHS_PARK";

    /// A child of this process that exec'd another program and works in the directory is a
    /// user with this process skipped: what it holds is its own. The program is a copy of
    /// this test binary at another path, run parked ([`PARK_ENV`]). Natively it is another
    /// executable; under a translator it is the translator, as this process's executable is
    /// (PRECONDITION: the case only [`super::own_program`] tells from an un-exec'd fork;
    /// with an equal executable taken as proof, the scan skipped this child, measured
    /// 2026-10-17). Negative control: the fork test above, where the same skip leaves a
    /// child out.
    #[test]
    fn a_child_that_execd_another_program_is_a_user_with_this_process_skipped() {
        const NAME: &str = "holders::paths::tests::\
            a_child_that_execd_another_program_is_a_user_with_this_process_skipped";
        if std::env::var_os(PARK_ENV).is_some() {
            let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0u8; 1]);
            return;
        }
        let dir = fresh("execd");
        let bin_dir = fresh("execd-bin");
        let bin = bin_dir.join("other");
        std::fs::copy(std::env::current_exe().expect("the test binary"), &bin)
            .expect("a copy of the test binary");
        let mut other = crate::spawnfd::command(&bin)
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(PARK_ENV, "1")
            .current_dir(&dir)
            .stdin(crate::spawnfd::Stdio::piped())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("the copy");
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

    /// A child's pid once the kernel shows it running `/bin/sleep`: its
    /// working directory and the descriptors it kept across the exec are
    /// its own from then on.
    #[cfg(target_vendor = "apple")]
    fn exec_done(child: &crate::spawnfd::Child) -> u32 {
        running(child, "/bin/sleep")
    }

    /// A child with no descriptor at all, working in `cwd`, stopped, and its pid once the stop
    /// is reported: a shell spawned with only its stdio (`spawnfd::command`) that closes those
    /// three and stops itself. SIGKILL and reap it with [`reap`].
    ///
    /// Spawned, not forked: a raw `fork` of this multithreaded binary can die in libSystem's
    /// atfork child handler (`_notify_fork_child` meeting an `os_once` another thread was in).
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
        // SAFETY: our own child; this waits for its stop, reaping nothing (the
        // handle reaps it in `reap`).
        let got = unsafe { proc_ffi::waitpid(pid, &raw mut status, WUNTRACED) };
        assert_eq!(got, pid, "waitpid");
        assert_eq!(status & 0xff, 0x7f, "stopped, not exited: {status:#x}");
        (child, pid)
    }

    /// SIGKILL and reap a [`bare_child`].
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

    /// A fresh PTY's master, unlocked, and its slave's path (`TIOCPTYGNAME`,
    /// what `ptsname` reads).
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

    /// A child holds a PTY slave open; `revoke(2)` on the slave's path (what a terminal's
    /// controlling process's exit does) reclaims the vnode under it, and the descriptor reads
    /// `ENOENT`. Scanned for that path, the child is then no user, not unreadable, and holds
    /// no device; before the revoke both scans find it. Negative control: with dead vnodes read
    /// as unknown, it is unknown to both scans.
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
        // The slave on descriptor 3, from a working directory and executable that are not it.
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
        let read = super::super::vnode_rdev(n, 3);
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

    /// A child works in a folder and holds a file elsewhere open; its working directory then
    /// reads `ENOENT`, as a reclaimed vnode's does once its volume is force-unmounted (the
    /// folder stands in for that volume: `hook::reading_cwd_dead`). Scanned for the folder it
    /// is then no user and not unreadable; scanned for the held file's directory it is a user
    /// through that descriptor. Negative control: with dead vnodes read as unknown, it is
    /// unreadable. The kernel's own answer under a real forced detach is
    /// [`a_force_detached_volume_is_no_working_directory`] (explicit acceptance bench).
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
        // In the folder, the file on descriptor 3; `cat` ends when this test's end of its
        // stdin closes, unwinding included.
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

    /// A process with no descriptor at all is read and holds nothing, and its working directory
    /// still counts. Negative control: a table the kernel refuses (launchd's, read because it
    /// is named) is unreadable.
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
            refused
                .unreadable
                .iter()
                .map(|(p, _)| *p)
                .collect::<Vec<_>>(),
            [1],
            "a refused table is unreadable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A process that exited after a listing named it (a zombie, reaped, or killed between its
    /// descriptors' listing and their reads) uses nothing and is never unreadable. Each was a
    /// user first.
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

    /// The firmlinked form of a data-volume path is the same path.
    #[test]
    fn a_data_volume_path_matches_its_firmlinked_form() {
        let dirs = [PathBuf::from("/Users//x/proj")];
        assert!(under(
            std::path::Path::new("/System/Volumes/Data/Users//x/proj/target"),
            &dirs
        ));
        assert!(under(std::path::Path::new("/Users//x/proj"), &dirs));
        assert!(!under(std::path::Path::new("/Users//x/project"), &dirs));
    }
}
