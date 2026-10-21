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
//! too. A use is reported in the spelling that placed it ([`PathUse::path`], on every
//! platform), so a caller placing it against one directory of several answers as the scan
//! did. Memory-mapped files that hold no descriptor are not seen.
//!
//! Windows reads it through the Win32/NT process APIs (`crate::win::ffi`), per same-user
//! process (the listing already filtered to this account's token user,
//! [`super::uid_pids`]): the image it runs (`QueryFullProcessImageNameW`), its working
//! directory (the PEB `ProcessParameters.CurrentDirectory.DosPath`, read with
//! `NtQueryInformationProcess` + `ReadProcessMemory`, WOW64 32-bit layouts handled: the one
//! reader of a process's parameters, `aterm_sysprobe::peb`), its loaded
//! modules (`EnumProcessModulesEx` + `GetModuleFileNameExW`: proc-macro DLLs map from
//! `target\*\deps`; the loader records a module's path as it was asked for, through any
//! junction or link, so one that places outside every directory is placed again by the
//! kernel's resolved name of the file mapped at its base, `GetMappedFileNameW`, spelled
//! through the mount table), and its open files: its handles of the `File` object type in one
//! `NtQuerySystemInformation` snapshot, each `DuplicateHandle`d in and named
//! (`GetFileType == FILE_TYPE_DISK`, then `GetFinalPathNameByHandleW`) on a helper thread
//! under a deadline, since on a synchronous file object the path query waits for the object's
//! lock, which the holder keeps while its own synchronous call on it runs (a blocking
//! `ReadDirectoryChangesW` — a watcher's, forever). A same-user process with a witness that
//! could not be read — it refuses the reads (an elevated one does, and so do `ctfmon` and
//! `taskhostw`), a file handle will not duplicate or name in time, a module's name or its
//! mapped file's will not read (or names a device the mount table mounts nowhere) — is
//! [`PathScan::unreadable`], named by its image, never a non-user; only one that has
//! exited (its pid gone, or its process handle signalled) holds nothing. The kernel spells a
//! path several ways (a verbatim `\\?\C:\x` handle path, a `C:\x\` working directory with a
//! trailing separator, a `C:\x` image), so every one is placed against the canonical
//! directories through [`win::placed`]: the one S1 rule of `aterm_sysprobe`
//! (`volumes::Table::is_under` over one reading of the mount table — a verbatim prefix
//! stripped, trailing separators dropped, case folded one unit to one, a `subst` letter
//! followed), after the Win32 normalization the loader's spellings need, and a path spelled
//! with 8.3 short names spelled long first (`volumes::Table::long_spelling_by`: each short
//! name by the long name its own directory lists, no link followed, asked only of a local
//! volume, on the naming helper under its deadline) — the spelling a use is then reported
//! in ([`PathUse::path`]). A path that cannot be placed (no drive
//! or share root, a lone surrogate, an 8.3 short name that could not be spelled long, a
//! share or a letter the table cannot relate to the directory) makes the process unreadable
//! — and, read once more after the whole pass, it stays so only if that read fails too; one
//! that refused the open itself is not read again (its security refuses a second open the
//! same way). Memory-mapped files that are not loaded modules and hold no handle are not
//! seen, as on the other platforms.
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
    /// The path, in the spelling the scan PLACED it at or under one of the
    /// directories by: what a caller that places this use against each of
    /// several directories compares (`aterm_agent`'s clause 4b over every
    /// tier-3 candidate, the stale-version keeps), so it answers what the
    /// scan answered. On Linux the kernel's text as it wrote it; on macOS
    /// that, or for a data-volume path (`/System/Volumes/Data/Users//x/t`)
    /// whose firmlinked form alone lies under a directory, that form
    /// (`/Users//x/t`); on Windows the kernel's text as Win32 normalizes it,
    /// each 8.3 short name spelled long (`\\?\C:\Users\x\Long Folder\t` for
    /// a working directory `C:\Users\x\LONGFO~1\t\`). The long spelling
    /// answers verbatim, so a use carries the `\\?\` prefix where a short
    /// name was spelled long — as a handle's path does, as the kernel names
    /// it — and is plain where the kernel wrote it so (`C:\x` for a working
    /// directory `C:\x\`); placement reads no prefix. The kernel's own text
    /// would not do: an 8.3 name places under no directory at all (the path
    /// key refuses it), so a caller holding what it cannot place held the
    /// use for EVERY directory, and a data-volume prefix places under none,
    /// so a caller dropped it for its own.
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

/// The kernel's `path` in the spelling that lies at or under one of `dirs`
/// ([`PathUse::path`]): as written, or — a data-volume path
/// (`/System/Volumes/Data/Users//x`) — by its firmlinked form (`/Users//x`),
/// as the canonical directories spell it. `None` when neither lies under any.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn placed(path: &Path, dirs: &[PathBuf]) -> Option<PathBuf> {
    if dirs.iter().any(|d| path.starts_with(d)) {
        return Some(path.to_path_buf());
    }
    let data = Path::new("/").join(path.strip_prefix("/System/Volumes/Data").ok()?);
    dirs.iter().any(|d| data.starts_with(d)).then_some(data)
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

/// See the macOS twin: every process of this uid, the listing
/// [`super::uid_pids`] (the session manager's, filtered to this token user).
///
/// # Errors
///
/// The process listing itself failing (the token user or the manager).
#[cfg(windows)]
pub fn scan_path_users(dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    scan_path_users_of(&super::uid_pids()?, dirs, skip)
}

/// See the macOS twin: every pid named is read, whatever its uid — a pid of
/// another account mostly refuses the open and reads [`PathScan::unreadable`].
/// One system-wide snapshot of open file handles serves the whole list; a
/// snapshot that cannot be read whole leaves every process's open files
/// unknown, so each reads unreadable (held). One helper thread names the
/// scan's handles under a deadline ([`win::Namer`]). One reading of the mount
/// table places every path the scan meets ([`win::placed`]); a table not read
/// whole places only what lies lexically under a directory, so every other
/// path holds its process.
///
/// A process that comes back unreadable is read once more after the whole
/// pass, through a fresh snapshot of open handles ([`settled`]), and held
/// only if that read fails too: most such reads catch a process mid-change
/// (starting, its module list or PEB not yet whole; exiting; a name query on
/// a handle it is closing), and one held for that alone made verdicts
/// flicker run to run. A handle closed between the snapshot and its
/// duplicate is no failure at all: the process holds no handle of that value
/// now, so it names nothing (`win::Dup::Closed`). One that refused the open itself (an
/// elevated process; the platform services every Windows 11 session runs)
/// is held at once and not read again: its security refuses a second open
/// the same way. So the second pass, and its snapshot, run only when a
/// process that granted the open failed a read.
///
/// # Errors
///
/// None: the signature is the macOS twin's.
#[cfg(windows)]
pub fn scan_path_users_of(pids: &[u32], dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    Ok(scan_naming(pids, dirs, skip, win::Namer::default()))
}

/// [`scan_path_users_of`] with each of its naming queries (a handle's path, a
/// mapped module's file, a short spelling's long one) waiting at most
/// `deadline` instead of the product's one second: the seam a test whose
/// POSITIVE answer needs every such query answered scans through, with a
/// one-minute hang detector as the deadline — a wait for something that
/// must happen is never a latency budget. A test seam.
///
/// # Errors
///
/// None: the signature is [`scan_path_users_of`]'s.
#[cfg(all(windows, any(test, debug_assertions)))]
pub fn scan_path_users_of_within(
    pids: &[u32],
    dirs: &[PathBuf],
    skip: &[u32],
    deadline: std::time::Duration,
) -> io::Result<PathScan> {
    Ok(scan_naming(pids, dirs, skip, win::Namer::within(deadline)))
}

/// The Windows scan of `pids` but `skip` ([`scan_path_users_of`]), its
/// handles, modules and short spellings named through `namer`.
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

/// The Windows scan of `pids` but `skip` ([`scan_path_users_of`]): `pass`
/// reads each process it is given (one snapshot of open handles per pass),
/// first every one, then — once, after the whole first pass — only those
/// that came back [`win::Read::Unreadable`]. The second read decides each of
/// those: a use makes it a user, no use or an exit (it holds nothing) clears
/// it, and only a second failure keeps it unreadable, with its image as
/// either read named it. Never a third pass: one that is still unreadable is
/// held, as before. A process that came back [`win::Read::Refused`] is held
/// at once and never read again, since its security refuses a second open
/// the same way; a first pass whose only failures are refusals is the only
/// pass. `scanned` counts the first pass. Product behaviour, as the Claude
/// census's settled re-read (`aterm-agent`'s `disk_discover`): the first
/// pass itself is the pause, so no sleep is taken.
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

/// See the macOS twin: the image `QueryFullProcessImageNameW` names, read with
/// the least access that names it (`PROCESS_QUERY_LIMITED_INFORMATION`, which an
/// elevated process of this account and a system helper that refuses the
/// scan's other reads both grant). It has exited when no process has its pid
/// (the open refuses with `ERROR_INVALID_PARAMETER`) or its handle is
/// signalled: one that exited while another process still holds a handle to it
/// keeps its pid and its image name, as a zombie does, and is
/// [`Exe::Exited`] all the same. Any other failure is [`Exe::Unnamed`]: it may
/// still run.
#[cfg(windows)]
#[must_use]
pub fn exe_of(pid: u32) -> Exe {
    win::exe_of(pid)
}

/// The Windows path-user scan's process reads. All the crate's Win32/NT calls
/// are declared in `crate::win::ffi`; this module only calls them.
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

    // OpenProcess access rights.
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const PROCESS_VM_READ: u32 = 0x0010;
    const PROCESS_DUP_HANDLE: u32 = 0x0040;
    /// Lets a wait on the process handle read its exit ([`exited`]).
    const SYNCHRONIZE: u32 = 0x0010_0000;
    /// The image and liveness reads: all an elevated process of this account,
    /// or a system helper that refuses the rest, grants.
    const LIMITED: u32 = PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE;
    /// Every read the scan makes: the image and liveness, the PEB and the
    /// module list (`VM_READ`), the open handles (`DUP_HANDLE`).
    const READ_ALL: u32 = LIMITED | PROCESS_VM_READ | PROCESS_DUP_HANDLE;
    /// The open's answer for a pid no process has.
    const ERROR_INVALID_PARAMETER: u32 = 87;
    /// A duplicate's answer for a handle value its process does not hold.
    const ERROR_INVALID_HANDLE: u32 = 6;
    /// A wait on a signalled (exited) process.
    const WAIT_OBJECT_0: u32 = 0;
    // NtQuerySystemInformation class: every open handle of every process.
    const SYSTEM_EXTENDED_HANDLE_INFORMATION: u32 = 64;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = i32::from_ne_bytes(0xC000_0004u32.to_ne_bytes());
    // EnumProcessModulesEx filter: every module, 32- and 64-bit.
    const LIST_MODULES_ALL: u32 = 0x03;
    // GetFileType results and the GetFinalPathNameByHandleW flag.
    const FILE_TYPE_UNKNOWN: u32 = 0x0000;
    const FILE_TYPE_DISK: u32 = 0x0001;
    const VOLUME_NAME_DOS: u32 = 0x0000;
    const DUPLICATE_SAME_ACCESS: u32 = 0x0002;

    /// How long one handle's type and path queries may take in the product's
    /// scan ([`Namer::default`]). A local query answers in microseconds; one
    /// blocked behind a synchronous call of its own process ([`handle_path`])
    /// may never answer, and its process reads as held.
    const NAME_DEADLINE: Duration = Duration::from_secs(1);

    /// What one process read found.
    pub(super) enum Read {
        /// A first use of a path under one of the directories.
        User(PathUse),
        /// Read, and no use under any directory.
        None,
        /// It exited before or during the read: it holds nothing.
        Exited,
        /// Its image, working directory, modules or handles could not be read
        /// after it granted the open, so it is not ruled out — with its image
        /// when that much was named.
        Unreadable(Option<PathBuf>),
        /// It refused the open for the scan's reads ([`refused`]: an elevated
        /// process of this account, a system helper that grants only the
        /// limited rights), so it is not ruled out — with its image when the
        /// limited open named it. Unlike [`Read::Unreadable`] it is never read
        /// again in one scan (`super::settled`): its security refuses a second
        /// open the same way.
        Refused(Option<PathBuf>),
    }

    /// A handle this module opened or duplicated, closed on drop.
    struct Handle(ffi::RawHandle);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a live handle this module opened.
            let _ = unsafe { ffi::CloseHandle(self.0) };
        }
    }
    impl Handle {
        /// The handle, lent to a reader that takes std's borrowed handle
        /// (`aterm_sysprobe::peb`'s image and working-directory reads).
        fn borrowed(&self) -> BorrowedHandle<'_> {
            // SAFETY: `self.0` is a live handle this module opened, closed
            // only when this `Handle` drops, which the borrow cannot outlive.
            unsafe { BorrowedHandle::borrow_raw(self.0 as std::os::windows::io::RawHandle) }
        }
    }

    /// Why [`open`] gave no handle.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Refusal {
        /// No process has the pid (`ERROR_INVALID_PARAMETER`): it has exited.
        Gone,
        /// The process is there and refuses this access.
        Denied,
    }

    /// Open `pid` with `access`.
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

    /// `pid` opened for its image and liveness alone ([`LIMITED`]), or for its
    /// image alone where `SYNCHRONIZE` is refused (then [`exited`] cannot read
    /// the exit and answers "running").
    fn open_limited(pid: u32) -> Result<Handle, Refusal> {
        match open(pid, LIMITED) {
            Err(Refusal::Denied) => open(pid, PROCESS_QUERY_LIMITED_INFORMATION),
            other => other,
        }
    }

    /// Has the process `h` exited? Only a signalled handle says so. A wait that
    /// fails (a handle opened without `SYNCHRONIZE`) answers "running", so a
    /// read that failed leaves its process held, never exited: a false
    /// "exited" would read a live process's directory as unused.
    fn exited(h: &Handle) -> bool {
        // SAFETY: `h.0` is a live process handle this module opened.
        let state = unsafe { ffi::WaitForSingleObject(h.0, 0) };
        state == WAIT_OBJECT_0
    }

    /// One witness's path as [`placed`] placed it against the directories.
    pub(super) struct Placed {
        /// The spelling it was placed by: the kernel's text as Win32
        /// normalizes it ([`win32_normalized`]), each 8.3 short name spelled
        /// long where that could be read (verbatim then, `\\?\C:\…`, as the
        /// long spelling answers) — the spelling a use is reported in
        /// ([`super::PathUse::path`]), so a caller that places it against
        /// one directory of several answers what the scan answered.
        pub(super) at: PathBuf,
        /// At or under one of the directories (`Some(true)`), under none of
        /// them (`Some(false)`), or `None`: it, or one of the directories,
        /// could not be placed.
        pub(super) under: Option<bool>,
    }

    /// A witness's placement ([`placed`]): its spelling when it lies at or
    /// under a directory, a hit (stop here); else `None`, and one that could
    /// not be placed raises `unreadable`.
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

    /// The first use `pid` makes of a path under `dirs`, each placed by `table`
    /// ([`placed`]). `handles` is this pid's open `File` handles from the system
    /// snapshot (`Some(&[])` when it holds none), or `None` when the snapshot
    /// itself could not be read; `namer` names each.
    ///
    /// A POSITIVE use always wins: every witness (image, working directory,
    /// modules, open files) is scanned for one at or under a directory, and
    /// only when none is found does a witness that could not be read — an image
    /// the open refuses to name, a PEB or module that would not read, a file
    /// handle that would not duplicate or name in time, a path that could not
    /// be placed — make the process [`Read::Unreadable`] (held), as the macOS
    /// twin's `unreadable` flag does. A process that refuses the reads is held
    /// too, named by its image ([`refused`], [`Read::Refused`]). One that has
    /// exited holds nothing ([`Read::Exited`]): read only from a signalled
    /// handle or a gone pid.
    pub(super) fn uses(
        pid: u32,
        dirs: &[PathBuf],
        table: &aterm_sysprobe::volumes::Table,
        handles: Option<&[usize]>,
        namer: &mut Namer,
    ) -> Read {
        // Shared with the namer's helper, which reads the process's mapped
        // files through it ([`Query::Mapped`]).
        let handle = match open(pid, READ_ALL) {
            Ok(h) => Arc::new(h),
            Err(Refusal::Gone) => return Read::Exited,
            Err(Refusal::Denied) => return refused(pid),
        };
        // One that exited while another process holds a handle to it still
        // opens, and names its image, but holds nothing.
        if exited(&handle) {
            return Read::Exited;
        }
        // A use is reported in the spelling it was placed by ([`Placed::at`]).
        let hit = |how: &'static str, path: PathBuf| Read::User(PathUse { pid, how, path });
        // Each witness: a placement at or under a directory is a hit (returned
        // at once); a path that could not be placed sets `unreadable`.
        let mut unreadable = false;

        // The image. A `None` here (the open refuses to name it) is itself an
        // unreadable witness; it also names the process in `unreadable`.
        let image = image_path(&handle);
        match &image {
            Some(p) => {
                if let Some(at) = note(placed(p, dirs, table, namer), &mut unreadable) {
                    return hit("exe", at);
                }
            }
            None => unreadable = true,
        }
        // The working directory.
        match read_cwd(&handle) {
            Some(cwd) => {
                if let Some(at) = note(placed(&cwd, dirs, table, namer), &mut unreadable) {
                    return hit("cwd", at);
                }
            }
            None => unreadable = true,
        }
        // Loaded modules (the main image again among them, already placed; a
        // proc-macro DLL from `deps\` is the one that matters here). The
        // loader records each module's path as it was asked for — through a
        // junction or a symbolic link, unresolved — and a mapped module holds
        // no handle that would name it resolved, so a module whose recorded
        // path lies outside every directory is placed again by the kernel's
        // name of the file mapped at its base ([`mapped_name`]), which the
        // open that made its section resolved. One whose recorded path
        // cannot be placed holds the process already.
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
        // Open files.
        match handles {
            Some(handles) => {
                for &value in handles {
                    let path = match duplicate(&handle, value) {
                        Dup::Of(dup) => namer.name((pid, Of::Handle(value)), Query::Handle(dup)),
                        // Closed since the snapshot listed it: it names nothing now.
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
                        // Not a file on a volume, or no handle at all now: no path.
                        HandlePath::NoFile => {}
                        // The duplicate or a query failed or did not answer
                        // in time: unknown.
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

    /// A process that refuses the scan's reads: an elevated process of this
    /// account, or a system helper that grants only the limited rights
    /// (`ctfmon`, `taskhostw` and `DefenderSessionHelper` on a Windows 11
    /// session refuse `VM_READ` and `DUP_HANDLE`, measured 2026-10-20). Held
    /// ([`Read::Refused`]), named by its image through the least access that
    /// reads it, unless it has exited.
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

    /// The image `h` runs, as `QueryFullProcessImageNameW` spells it
    /// (`C:\...\x.exe`, `aterm_sysprobe::peb::image_of`), or `None` when the
    /// call fails.
    fn image_path(h: &Handle) -> Option<PathBuf> {
        aterm_sysprobe::peb::image_of(h.borrowed())
    }

    /// `h`'s loaded modules, each by its base address and the path the loader
    /// recorded (`EnumProcessModulesEx` + `GetModuleFileNameExW`), and whether
    /// every one was named; `None` when the enumeration fails. An empty list
    /// is a valid answer (a process still starting has none yet). A module
    /// whose name will not read may be the one under the directory — a
    /// proc-macro DLL's file handle is closed once it is mapped, so the module
    /// list is its only witness — so the caller holds the process unless
    /// another witness is a hit.
    fn modules(h: &Handle) -> Option<(Vec<(usize, PathBuf)>, bool)> {
        let mut need: u32 = 0;
        // Size, then fill with headroom; a full array is re-asked with more room.
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
                // 0 is a failure, and a full buffer a name cut short.
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

    /// The kernel's name of the file mapped at `base` in `process`
    /// (`GetMappedFileNameW`: `\Device\HarddiskVolume3\x\y.dll`), the name of
    /// the file the section was made from as the open that made it resolved
    /// it — every junction and symbolic link on the way followed, which the
    /// loader's record of the module ([`modules`]) never is. [`on_volume`]
    /// spells it for [`placed`]. Run on a [`Namer`] helper: the query asks the
    /// file's own file system for its name, which a redirector may answer
    /// slowly. [`HandlePath::Failed`] when the call fails (nothing is mapped
    /// there now: the module was unloaded since it was listed) or the name
    /// fills the buffer.
    fn mapped_name(process: &Handle, base: usize) -> HandlePath {
        let mut buf = vec![0u16; 32768];
        // SAFETY: `process.0` is a live process handle opened for the scan's
        // reads ([`READ_ALL`]); `base` is an address in that process, which
        // the kernel reads and this process never dereferences; `buf` is its
        // capacity in UTF-16 units.
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

    /// The kernel's name of a file (`\Device\HarddiskVolume3\x\y.dll`,
    /// [`mapped_name`]) spelled through the mount point `table` lists for its
    /// device (`\\?\C:\x\y.dll`; a folder mount point's `\\?\C:\mnt\d\x\y.dll`),
    /// which [`placed`] then relates to every other mount point of the volume.
    /// A `subst` letter names a folder, never a device, so it is never the one.
    /// `None` for a device `table` mounts nowhere — a share's redirector
    /// (`\Device\Mup\…`), a shadow copy, a volume mounted since the table was
    /// read — or a name that is not Unicode: the caller holds the process.
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

    /// `h`'s working directory, from its PEB (the 32-bit one under WOW64,
    /// `aterm_sysprobe::peb::cwd_of`), or `None` when any read fails.
    fn read_cwd(h: &Handle) -> Option<PathBuf> {
        aterm_sysprobe::peb::cwd_of(h.borrowed())
    }

    /// What a process's open file handle names ([`handle_path`]), or the file
    /// mapped at one of its modules' bases ([`mapped_name`]: `At` or `Failed`).
    enum HandlePath {
        /// A disk file or directory, by its final path (a mapped file by the
        /// kernel's name, `\Device\HarddiskVolume3\…`).
        At(PathBuf),
        /// No file: not a disk object (a pipe, socket or console), or a disk
        /// device opened itself ([`disk_device_itself`]).
        NoFile,
        /// The duplicate or a query failed, or did not answer in time.
        Failed,
    }

    /// What [`duplicate`] made of one of a process's open handles.
    enum Dup {
        /// The duplicate, which names what the handle names.
        Of(Handle),
        /// The process has no handle of that value now (`ERROR_INVALID_HANDLE`,
        /// asked with a process handle that grants the duplicate): it was closed
        /// since the snapshot listed it, and names nothing. A busy build or
        /// test process closes handles faster than one scan reads them: in 30
        /// whole-account scans beside running builds and test binaries
        /// (measured 2026-10-20), 29 of the 31 failed handle reads were this,
        /// and each held its process as unreadable.
        Closed,
        /// The duplicate failed otherwise (a handle that refuses it): unknown.
        Failed,
    }

    /// `h`'s open handle `value`, duplicated into this process ([`Dup`]). It
    /// never blocks: no file system is asked.
    fn duplicate(h: &Handle, value: usize) -> Dup {
        let mut dup: ffi::RawHandle = 0;
        // SAFETY: source handle `value` is a handle of `h`'s process (or, closed
        // since the snapshot, a failure); the target is this process; the
        // duplicate is owned by the returned `Handle`.
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

    /// What the duplicated file handle `dup` names: its final path when it is
    /// a disk file or directory. Run on a [`Namer`] helper, never on the
    /// scan's thread: on a synchronous file object the path query waits for
    /// the object's lock, which a thread of the holding process keeps for as
    /// long as its own synchronous call on that object runs (a blocking
    /// `ReadDirectoryChangesW` — a watcher's, forever — a `LockFileEx`, a read
    /// stuck on a network file). `GetFileType` answered at once on such a
    /// handle (measured 2026-10-20), but every call that touches the holder's
    /// file object runs here all the same.
    fn handle_path(dup: &Handle) -> HandlePath {
        // `GetFileType` answers FILE_TYPE_UNKNOWN both for a device of no known
        // type and when it fails; only a failure leaves an error behind.
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
        // SAFETY: `dup.0` is a live disk handle; `buf` is its capacity. The
        // call returns the length written, 0 on failure, or more than the
        // capacity when the path is longer (then nothing was written).
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
            // A disk device opened itself (a volume, `\\.\C:`; a whole disk)
            // has no file name for any name query to answer (measured: every
            // form fails with ERROR_INVALID_FUNCTION); its kernel name says so.
            _ if object_name(dup).is_some_and(|name| disk_device_itself(&name)) => {
                HandlePath::NoFile
            }
            _ => HandlePath::Failed,
        }
    }

    /// The kernel name of the object `h` names (`NtQueryObject`,
    /// `ObjectNameInformation`): for a file, its device's name and then the
    /// file's path on it. `None` when the query fails or the name is not
    /// UTF-16 text.
    fn object_name(h: &Handle) -> Option<String> {
        const OBJECT_NAME_INFORMATION: u32 = 1;
        // A `UNICODE_STRING` header, then the characters it points at: kept
        // as `u64`s so the header is aligned.
        let mut buf = vec![0u64; 8192];
        let bytes = u32::try_from(buf.len() * 8).ok()?;
        let mut ret: u32 = 0;
        // SAFETY: `h.0` is a live handle; `buf` is `bytes` writable bytes;
        // `ret` is a live local.
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
        // SAFETY: the call wrote a `UnicodeString` at the start of `buf`,
        // which is aligned for it and larger than it.
        let name: ffi::UnicodeString = unsafe { std::ptr::read(buf.as_ptr().cast()) };
        let base = buf.as_ptr() as usize;
        // The characters lie inside `buf`, after the header; anything else is
        // not read.
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

    /// Is `name`, a file object's kernel name, a disk device itself — a volume
    /// (`\Device\HarddiskVolume3`, what `\\.\C:` opens), a whole disk or one of
    /// its partitions (`\Device\Harddisk0\DR0`, `\Device\Harddisk0\Partition1`)
    /// or an optical drive (`\Device\CdRom0`) — with no file below it? A file
    /// on one of them is named with its path after the device, so it never
    /// matches; anything else that will not name stays unknown.
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

    /// Which of one process's queries a key names ([`STUCK`], [`Namer::name`]).
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Of {
        /// Its open handle of this value ([`handle_path`]).
        Handle(usize),
        /// The file mapped at this module base ([`mapped_name`]).
        Module(usize),
        /// The long spelling of one short-spelled path ([`Namer::long_name`]),
        /// by [`spelling_key`] of its text: no process's own query, so keyed
        /// under [`NO_PROCESS`].
        Spelling(u64),
    }

    /// The pid a query that is no process's own is keyed by ([`Of::Spelling`]):
    /// 0, the idle process's, which no scan reads. A path stuck behind a
    /// stalled server then answers unknown at once for every process that
    /// reports it, not after a deadline per process.
    const NO_PROCESS: u32 = 0;

    /// [`Of::Spelling`]'s key of `path`: its hash, by std's hasher with its
    /// fixed keys, so one path keys alike for the life of the process. Two
    /// paths that collide share one entry in [`STUCK`]: while one is stuck,
    /// the other answers unknown too — held, the safe side.
    fn spelling_key(path: &std::path::Path) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::hash::DefaultHasher::new();
        path.hash(&mut hasher);
        hasher.finish()
    }

    /// The queries that outlived their namer's deadline and still block an
    /// abandoned helper, by (pid, which query). One asked again answers unknown at once,
    /// so a watcher's permanently pending call strands one thread, never one
    /// per scan; the helper removes its entry when its query returns (the
    /// holder's call completed, or its process ended).
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

    /// `f`'s answer, run while the long spelling of `through` is marked stuck
    /// in [`STUCK`], as one whose helper outlived its deadline is: the
    /// seam a test reads [`Namer::long_name`]'s routing through. The mark is
    /// removed once `f` returns or panics.
    #[cfg(test)]
    pub(super) fn with_spelling_stuck<R>(through: &std::path::Path, f: impl FnOnce() -> R) -> R {
        /// Removes its key from [`STUCK`] when dropped.
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

    /// Whether a [`Namer`]'s helper thread holds the system's error boxes off
    /// while it names ([`aterm_sysprobe::errors_quiet`], read ON the helper
    /// through a [`Query::Probe`]); `None` when the helper never answered
    /// within `deadline` (the caller's hang detector).
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

    /// Names one scan's file handles ([`handle_path`]) and mapped modules
    /// ([`mapped_name`]), and spells its short-spelled paths long
    /// ([`Namer::long_name`]), on a helper thread under its deadline
    /// ([`NAME_DEADLINE`] in the product's scan), so a query that blocks
    /// costs its own process a "held", never the scan. The helper starts with
    /// the first query, is replaced after one it abandons, and ends when the
    /// namer is dropped.
    ///
    /// An abandoned helper keeps its duplicate of the handle until its query
    /// returns, and the duplicate keeps the holder's file object alive. A
    /// synchronous call pending on the handle is never ended by a close, with
    /// or without the duplicate (only its own completion or a cancel ends
    /// it); what the duplicate changes is the holder's close of its handle,
    /// which then returns at once instead of blocking behind that call, the
    /// file object living on until the helper's query returns.
    pub(super) struct Namer {
        helper: Option<Helper>,
        /// How long one query may take before it is abandoned and its
        /// process read as held: [`NAME_DEADLINE`] for every namer the
        /// product makes ([`Namer::default`]). A test whose positive answer
        /// needs every query answered gives a one-minute hang detector
        /// instead ([`Namer::within`]): a wait for something that must happen
        /// is never a latency budget. A test that needs the deadline to fire
        /// gives a short one of its own, and asserts only what fired, never
        /// how long it took.
        deadline: Duration,
    }

    impl Default for Namer {
        /// The product's namer: each query under [`NAME_DEADLINE`].
        fn default() -> Self {
            Self::within(NAME_DEADLINE)
        }
    }

    /// One helper thread's channels, and the flag that says its caller gave up
    /// on it (read and written under the [`STUCK`] lock).
    struct Helper {
        jobs: mpsc::Sender<Job>,
        answers: mpsc::Receiver<HandlePath>,
        abandoned: Arc<AtomicBool>,
    }

    /// What one helper query reads.
    enum Query {
        /// The duplicate of a process's open handle, which the helper names
        /// ([`handle_path`]) and then closes.
        Handle(Handle),
        /// A process (the scan's own handle to it, shared) and the base of one
        /// of its modules: the file mapped there ([`mapped_name`]).
        Mapped(Arc<Handle>, usize),
        /// The verbatim part of a short-spelled path through its last short
        /// name, which the helper spells long
        /// ([`aterm_sysprobe::volumes::long_name`], [`Namer::long_name`]).
        Spell(PathBuf),
        /// A test's own question, answered on the helper thread
        /// ([`helper_errors_quiet`]).
        #[cfg(test)]
        Probe(fn() -> HandlePath),
    }

    /// One query: whose it is, and what the helper reads.
    struct Job {
        key: (u32, Of),
        query: Query,
    }

    impl Namer {
        /// A namer whose queries each wait at most `deadline` ([`Namer`]'s
        /// `deadline`: the product's is [`NAME_DEADLINE`]).
        pub(super) fn within(deadline: Duration) -> Self {
            Self {
                helper: None,
                deadline,
            }
        }

        /// What `query`, query `key.1` of process `key.0`, names;
        /// [`HandlePath::Failed`] when the query did not answer within this
        /// namer's deadline, is still stuck from an earlier scan, or no
        /// helper thread could start.
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
                    // Under the lock the helper answers under: it has either
                    // answered already (the answer waits in the channel) or
                    // will find itself abandoned and remove the entry.
                    let mut stuck = STUCK.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Ok(path) = helper.answers.try_recv() {
                        drop(stuck);
                        self.helper = Some(helper);
                        return path;
                    }
                    helper.abandoned.store(true, Ordering::Relaxed);
                    stuck.push(key);
                    // Dropping `helper` closes its job channel: the thread ends
                    // once its query returns.
                    HandlePath::Failed
                }
            }
        }

        /// The long spelling of `through`, the existing verbatim part of a
        /// short-spelled path through its last short name
        /// ([`aterm_sysprobe::volumes::long_name`], which
        /// `Table::long_spelling_by` asks for in [`placed`]), read on the
        /// helper under this namer's deadline. The table placed only the part
        /// before the first short name on a local volume; a link after it can
        /// lead to a share, whose listing waits as long as its server does.
        /// Here such a wait costs the processes that report the path a "held"
        /// and the scan one deadline, then nothing while it stays stuck
        /// ([`NO_PROCESS`]). `None` when the call failed, did not answer in
        /// time, or is still stuck from an earlier scan.
        fn long_name(&mut self, through: &std::path::Path) -> Option<PathBuf> {
            let key = (NO_PROCESS, Of::Spelling(spelling_key(through)));
            match self.name(key, Query::Spell(through.to_path_buf())) {
                HandlePath::At(long) => Some(long),
                HandlePath::NoFile | HandlePath::Failed => None,
            }
        }
    }

    impl Helper {
        /// A helper thread waiting for jobs, or `None` when none could start.
        #[cfg(not(target_arch = "wasm32"))]
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

    /// A helper's loop: each job's [`handle_path`], [`mapped_name`] or long
    /// spelling, answered under the [`STUCK`] lock, so a caller that gives up
    /// sees either the answer or a helper that removes its own entry. It ends
    /// when the namer drops its job channel, or once the query its caller
    /// abandoned returns.
    ///
    /// For its whole life it holds the system's error boxes off
    /// ([`aterm_sysprobe::quiet_errors`]): each query asks a file's own file
    /// system for its name, and one on an empty card reader or optical drive
    /// otherwise puts a "There is no disk in the drive" box on the owner's
    /// desktop and parks the helper until someone answers it.
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
            // A namer dropped since (its scan ended) leaves no one to answer.
            let _ = answers.send(path);
        }
    }

    /// Every process's open handles of the `File` object type — files,
    /// directories, pipes, devices — from one system snapshot
    /// ([`handle_snapshot`]), grouped by pid, or `None` when the snapshot
    /// cannot be read whole: then no process's open files can be ruled out.
    /// Handles of the other types (events, keys, sections, ETW registrations)
    /// name no file, and most refuse a duplicate: read as unknown, they would
    /// hold every process. The `File` type's index is assigned per boot, so it
    /// is learned from a handle this process opens (`NUL`, which asks no file
    /// system) and finds in the same snapshot.
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

    /// Every open handle of every process
    /// (`NtQuerySystemInformation(SystemExtendedHandleInformation)`: a
    /// two-word header whose first word counts the entries, then the entries),
    /// or `None` when the query fails or counts more entries than its buffer
    /// holds (an incomplete snapshot rules nothing out).
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

    /// Is the kernel's `path` at or under one of `dirs` (each canonical, so a
    /// verbatim `\\?\C:\...` spelling), as the mount table `table` places the
    /// two ([`aterm_sysprobe::volumes::Table::is_under`], THE S1 rule: any
    /// spelling, any case, a `subst` letter followed, a volume's every mount
    /// point)? `None` when `path`, or any of `dirs`, cannot be placed — the
    /// caller reads that as held: a name the key refuses (an 8.3 short name
    /// that could not be spelled long among them), a share or network letter
    /// not lexically under the directory (a share can be the local disk
    /// again), a drive letter the table does not list, or any comparison in a
    /// table that is not complete. A Win32 path is first normalized as Windows
    /// itself normalizes it ([`win32_normalized`]).
    ///
    /// A path spelled with 8.3 short names (a process started in
    /// `C:\Users\ANDREW~1\x` keeps that spelling as its working directory,
    /// and may as its image and modules) is spelled long first, by the one
    /// rule ([`aterm_sysprobe::volumes::Table::long_spelling_by`]: each short
    /// name by the long name its own directory lists, no link followed, asked
    /// only of a local volume), so it places where its long spelling places
    /// — under its folder, and NOT under every other one, where unplaced it
    /// held the process as unreadable for every directory any scan asked
    /// about. The file system is asked on `namer`'s helper under its deadline
    /// ([`Namer::long_name`]): a link after the placed part can lead to a
    /// share that stalls. One that cannot be expanded in time stays
    /// unplaced: held.
    ///
    /// Still lexical: a junction or a symbolic link in a recorded path is not
    /// resolved, whichever way it is spelled. So each witness is also read as
    /// the kernel resolved it: the image as `QueryFullProcessImageNameW`
    /// names it (its section's file), the working directory through the
    /// process's handle to it (named long and resolved, among its open
    /// files), and a module the loader recorded through a link by the
    /// kernel's name of the file mapped at its base ([`mapped_name`],
    /// [`on_volume`]).
    ///
    /// The answer carries the spelling it placed (normalized, spelled long:
    /// [`Placed::at`]), and a use is reported in it, so a caller that places
    /// the use again — against each of several directories, through the same
    /// rule — answers what this did, with no file system asked a second time.
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

    /// `path` as Win32 normalizes a path that is not verbatim before any file
    /// system sees it: `/` is a separator, runs of separators collapse, `.`
    /// names drop and `..` removes the name before it, never climbing above
    /// the root (a drive's `X:\`, a share's `\\server\share`). The loader keeps
    /// a module's path as its creator spelled it, so this happens: Git for
    /// Windows runs `C:\Program Files\Git\bin\..\usr\bin\bash.exe` (measured
    /// 2026-10-20), which the key refuses as written. A verbatim path
    /// (`\\?\`, `\??\`) reaches NTFS as written and comes back unchanged, as
    /// does anything rooted at neither a drive nor a share (the key refuses
    /// those: held) and a path that is not Unicode. A name ending in `.` or a
    /// space is left as it is: Win32 trims those by rules the key does not
    /// repeat, so it refuses them.
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
///
/// # Errors
///
/// Always `Unsupported`.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
pub fn scan_path_users(dirs: &[PathBuf], skip: &[u32]) -> io::Result<PathScan> {
    scan_path_users_of(&[], dirs, skip)
}

/// See the macOS twin.
///
/// # Errors
///
/// Always `Unsupported`.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
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
        // The scan takes canonical folders, as the kernel names them: a caller's temporary
        // folder is `/var/folders/…`, which the kernel reads as `/private/var/folders/…`.
        let base = base.canonicalize()?;
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
    use super::{PathBuf, placed, scan_path_users};
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
    /// kernel kills it, and deleted only once the child has echoed a line back
    /// ([`crate::deadline::cat_echoes`], the proof `ad_hoc_copy` asks for). The child ends at
    /// its EOF.
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

    /// Present in the environment of the next test's child: say [`PARKED`], then hold until
    /// stdin closes.
    const PARK_ENV: &str = "ATERM_UDS_PATHS_PARK";

    /// The line the parked child writes once it runs: the proof its copy ran
    /// ([`crate::deadline::prints`]).
    const PARKED: &str = "parked";

    /// A child of this process that exec'd another program and works in the directory is a
    /// user with this process skipped: what it holds is its own. The program is a copy of
    /// this test binary at another path, run parked ([`PARK_ENV`]). Natively it is another
    /// executable; under a translator it is the translator, as this process's executable is
    /// (PRECONDITION: the case only [`super::own_program`] tells from an un-exec'd fork;
    /// with an equal executable taken as proof, the scan skipped this child, measured
    /// 2026-10-17). The copy is a new file macOS assesses on its first exec, so it goes only
    /// once the child wrote [`PARKED`] (its working directory and executable path show
    /// while it still waits for the verdict); a child that never does leaves it in place.
    /// Negative control: the fork test above, where the same skip leaves a child out.
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

    /// The firmlinked form of a data-volume path is the same path, and the
    /// use is spelled by the form that placed it ([`super::PathUse::path`]):
    /// a caller placing it against its directory compares that, which the
    /// kernel's data-volume text never lies under. NEGATIVE CONTROLS: a path
    /// that places as written keeps its spelling; a sibling sharing a prefix
    /// places nowhere.
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

    /// The hang detector: how long a test waits for something that must
    /// happen, and so the deadline every naming query of a test whose answer
    /// needs it answered is given ([`scan_of`], [`Namer::within`]). This crate
    /// is dependency-free (and `aterm-spec` depends on it), so this is the
    /// local twin of `aterm_spec::wait::MUST_HAPPEN`.
    const HANG: Duration = Duration::from_secs(60);

    /// [`scan_path_users_of`] with each naming query (a handle's path, a
    /// mapped module's file, a short spelling's long one) waiting up to the
    /// hang detector ([`HANG`]), not the product's one second: what every
    /// test scans through whose positive answer — a use found, or a child
    /// read cleanly — needs every name read. The product's deadline is the
    /// subject only of the test that needs it to fire
    /// (`a_child_blocked_in_a_synchronous_watch_is_held_and_the_scan_returns`).
    fn scan_of(pids: &[u32], dirs: &[PathBuf], skip: &[u32]) -> std::io::Result<super::PathScan> {
        super::scan_path_users_of_within(pids, dirs, skip, HANG)
    }

    /// A complete mount table of two local drives, `C:\` and `D:\`, and the
    /// network's synthetic mount, as `aterm_sysprobe::volumes::table` lists
    /// them: what the pure placement tests place against.
    fn two_drives() -> Table {
        let volume = |on: &str, n: u32, guid: &str| Mount {
            on: PathBuf::from(on),
            from: format!(r"\Device\HarddiskVolume{n}"),
            fstype: String::new(),
            local: true,
            automounted: false,
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

    /// [`super::win::placed`] against [`two_drives`], with a namer of its own
    /// whose queries wait up to the hang detector.
    fn placed(path: &Path, dirs: &[PathBuf]) -> Option<bool> {
        super::win::placed(path, dirs, &two_drives(), &mut Namer::within(HANG)).under
    }

    /// [`super::win::placed`] against this machine's own mount table: a
    /// kernel path a live child reported.
    fn placed_here(path: &Path, dirs: &[PathBuf]) -> Option<bool> {
        placement_here(path, dirs).under
    }

    /// [`placed_here`]'s whole answer: the spelling it placed, and where.
    fn placement_here(path: &Path, dirs: &[PathBuf]) -> super::win::Placed {
        let table = aterm_sysprobe::volumes::table();
        super::win::placed(path, dirs, &table, &mut Namer::within(HANG))
    }

    /// Present in a parked child's environment: it then sleeps instead of
    /// running the test body, so this process can read it as a live user of a
    /// directory ([`spawn_parked`]). Run without it (the ordinary suite) the
    /// test is a no-op that passes.
    const PARK_ENV: &str = "ATERM_UDS_WIN_PARK";
    /// When set beside [`PARK_ENV`], a file the parked child opens and holds
    /// open for its whole life, so this process reads it as a held file.
    const PARK_HOLD: &str = "ATERM_UDS_WIN_HOLD";
    /// When set beside [`PARK_ENV`], a module the parked child maps (none of
    /// its code runs), so this process reads it among the child's modules.
    const PARK_LOAD: &str = "ATERM_UDS_WIN_LOAD";
    /// When set beside [`PARK_ENV`], a directory the parked child opens
    /// synchronously and then waits on in `ReadDirectoryChangesW` for the rest
    /// of its life, holding that file object's lock as a watcher does.
    const PARK_WATCH: &str = "ATERM_UDS_WIN_WATCH";
    /// When set beside [`PARK_ENV`], a file the parked child creates once it
    /// has set up everything else it was asked for: its parent reads it then.
    const PARK_READY: &str = "ATERM_UDS_WIN_READY";
    /// When set beside [`PARK_ENV`], a device the parked child opens with no
    /// access (a volume, `\\.\C:`, which any account may open so) and holds.
    const PARK_DEVICE: &str = "ATERM_UDS_WIN_DEVICE";
    /// When set beside [`PARK_ENV`], the parked child denies everyone the
    /// scan's memory and handle reads of it (`PROCESS_VM_READ`,
    /// `PROCESS_DUP_HANDLE`) and grants only the image and liveness reads, as
    /// `ctfmon` and `taskhostw` do.
    const PARK_DENY: &str = "ATERM_UDS_WIN_DENY";

    /// What a parked child does before it sleeps ([`park_mode_sleeps`]).
    #[derive(Default)]
    struct Park<'a> {
        /// A file it holds open ([`PARK_HOLD`]).
        hold: Option<&'a Path>,
        /// A module it maps ([`PARK_LOAD`]).
        load: Option<&'a Path>,
        /// A directory it blocks watching ([`PARK_WATCH`]).
        watch: Option<&'a Path>,
        /// A file it creates when set up ([`PARK_READY`]).
        ready: Option<&'a Path>,
        /// A device it holds open with no access ([`PARK_DEVICE`]).
        device: Option<&'a Path>,
        /// Whether it refuses the scan's reads of it ([`PARK_DENY`]).
        deny: bool,
    }

    /// The parked body: a child spawned with [`PARK_ENV`] set holds any
    /// [`PARK_HOLD`] file open, maps any [`PARK_LOAD`] module, says it is set
    /// up through any [`PARK_READY`] file, blocks watching any [`PARK_WATCH`]
    /// directory, and sleeps past the hang detector (its parent kills it once
    /// it has been read); run normally it does nothing.
    #[test]
    fn park_mode_sleeps() {
        use std::os::windows::ffi::OsStrExt as _;
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::os::windows::io::AsRawHandle as _;
        /// `LoadLibraryExW`: map the module, run none of it.
        const DONT_RESOLVE_DLL_REFERENCES: u32 = 0x0000_0001;
        /// `CreateFileW`: open a directory.
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        /// `ReadDirectoryChangesW`: names, attributes, sizes, writes.
        const NOTIFY_ALL: u32 = 0x0000_001F;
        if std::env::var_os(PARK_ENV).is_none() {
            return;
        }
        if std::env::var_os(PARK_DENY).is_some() {
            /// `SDDL_REVISION_1`.
            const SDDL_REVISION: u32 = 1;
            /// `DACL_SECURITY_INFORMATION`.
            const DACL: u32 = 0x0000_0004;
            // Deny everyone VM_READ | DUP_HANDLE (0x50); grant everyone
            // QUERY_LIMITED_INFORMATION | SYNCHRONIZE (0x101000) and no more.
            let sddl: Vec<u16> = "D:(D;;0x50;;;WD)(A;;0x101000;;;WD)"
                .encode_utf16()
                .chain([0])
                .collect();
            let mut sd: *mut core::ffi::c_void = std::ptr::null_mut();
            // SAFETY: `sddl` is NUL-terminated; `sd` receives a descriptor the
            // system allocates, kept for this process's life.
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
            // SAFETY: the pseudo-handle names this process; `sd` is the valid
            // descriptor built above.
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
            // SAFETY: `wide` is a NUL-terminated path; no code of the module
            // runs, and it stays mapped for this process's life.
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
            // DWORD-aligned, as the call requires.
            let mut buf = [0u32; 1024];
            let mut got: u32 = 0;
            // SAFETY: `dir` is a live directory handle opened synchronously;
            // `buf` is 4096 writable aligned bytes; no OVERLAPPED, so the call
            // blocks until a change under the directory (none comes).
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

    /// A fresh directory of this test's, canonical (so a `\\?\C:\...` spelling,
    /// as the directories a scan is given are).
    fn fresh(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("aterm-uds-winpaths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        std::fs::canonicalize(&d).expect("canonical")
    }

    /// A parked copy of this test binary ([`park_mode_sleeps`]) with `cwd` as
    /// its working directory and, when given, `hold` as a file it opens and
    /// keeps open for its whole life.
    fn spawn_parked(cwd: &Path, hold: Option<&Path>) -> Parked {
        spawn_parked_with(
            cwd,
            &Park {
                hold,
                ..Park::default()
            },
        )
    }

    /// A parked child, killed and reaped when dropped — after a failed
    /// assertion too, so a child that blocks for good (a watch) never outlives
    /// its test.
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

    /// A parked copy of this test binary ([`park_mode_sleeps`]) with `cwd` as
    /// its working directory, doing what `park` asks before it sleeps.
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

    /// Wait (the hang detector) until the parked `child` has created its
    /// `ready` file: everything it was asked to set up is in place.
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

    /// Scan until `want` holds for the child `pid`, or the hang detector fires
    /// (a spawned child shows its working directory at once, but its opened
    /// files take a moment). Fails fast if the child exited (a wrong `--exact`
    /// name, say).
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

    /// The path as `cmd` takes it: the canonical spelling without its verbatim
    /// prefix (test input only, never an operation's path).
    fn for_cmd(path: &Path) -> String {
        let s = path.to_str().expect("a test path is UTF-8");
        s.strip_prefix(r"\\?\").unwrap_or(s).to_owned()
    }

    /// A child working under the directory uses it ("cwd"). Negative control: a
    /// directory the child is not in has no user, and this process (skipped)
    /// is neither read nor counted — the scan names only the child and this
    /// process, never the account's other processes.
    #[test]
    fn a_child_working_under_the_directory_uses_it() {
        let dir = fresh("cwd");
        // Chained joins, so the path keeps `\` separators: a `/` would ride the
        // canonical `\\?\` prefix as a literal character and place nowhere.
        let deep = dir.join("target").join("debug").join("deps");
        std::fs::create_dir_all(&deep).expect("deps");
        let mut worker = spawn_parked(&deep, None);
        let how = until(std::slice::from_ref(&dir), &mut worker, |h| h.is_some());

        // Negative control: a directory the child does not use.
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

    /// A CHILD STARTED IN THE 8.3 SPELLING OF ITS FOLDER (live1's first
    /// finding: the spelling a short `%TEMP%` hands every child) keeps that
    /// spelling as its working directory, and the scan spells it long by the
    /// one rule (`Table::long_spelling`): the child is a user of its folder
    /// BY ITS WORKING DIRECTORY ("cwd"; unplaced, only its handle to the
    /// directory named it, "fd"), and NOT unreadable for another folder,
    /// which it was for every folder while its working directory placed
    /// nowhere. Scanned over BOTH folders at once, its use is reported in the
    /// spelling the scan placed it by, so a caller placing it against each
    /// folder by the same rule (clause 4b over every tier-3 candidate) finds
    /// it under its own and NOT under the other — reported as the kernel
    /// wrote it, it placed under neither and was held for both (wave 5's
    /// live finding F1). NEGATIVE CONTROL: the working directory it reports
    /// is short and places nowhere by the table alone. PREMISE, stated where
    /// it fails: the volume generates short names.
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

    /// A CHILD WORKING THROUGH A JUNCTION SPELLED SHORT USES THE DIRECTORY THE
    /// JUNCTION IS IN, as the same working directory spelled long does (the
    /// 8.3 branch's review, rev4-1: the short spelling was expanded by
    /// following the link, so the child placed at the junction's target and
    /// was neither a user nor unreadable for the candidate). The expansion
    /// keeps the junction's own long name, so the recorded working directory
    /// is a "cwd" use of the candidate. NEGATIVE CONTROL: for the junction's
    /// target the recorded spelling is no use, and the child is a user there
    /// only by its handle to the directory, which the kernel names resolved
    /// ("fd"): each witness reads as what it is. PREMISE, stated where it
    /// fails: the volume generates short names.
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

    /// A SHORT SPELLING IS SPELLED LONG ON THE NAMING HELPER, under its
    /// deadline (the 8.3 branch's review, rev4-2: the scan's own thread asked
    /// the file system, with no deadline, though a link after the part the
    /// table places can lead to a share that stalls): an expansion stuck
    /// there holds its path — unplaced — at once, without asking again.
    /// NEGATIVE CONTROL: the same path, nothing stuck, places under its
    /// folder, so the hold is the stuck query's, and in its long spelling
    /// (what a use is reported in), the names after the short one appended
    /// as written. PREMISE, stated where it fails: the volume generates
    /// short names.
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

    /// A PROCESS UNREADABLE BY CHANCE IS NOT HELD (live1's second minor
    /// finding: claude.exe, aterm-gui.exe or VCTIP.EXE unreadable in 3 of 10
    /// scans, read cleanly a moment later, so a version row or a tier-3 row
    /// flickered between held and removable): after the whole first pass,
    /// only the processes that came back unreadable are read once more
    /// ([`super::settled`]) and the second read decides — no use clears one,
    /// an exit clears one, a use makes one a user — while a skipped pid is
    /// read by neither pass and a pass with nothing unreadable is the only
    /// one. NEGATIVE CONTROL: a process that fails both reads is still held,
    /// named by the image its first read gave when the second gave none.
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

    /// A PROCESS THAT REFUSES THE OPEN IS HELD AT ONCE AND NEVER READ AGAIN
    /// (the 8.3 branch's review, rev4-4: `ctfmon`, `taskhostw` and
    /// `DefenderSessionHelper` refuse every scan on a Windows 11 session, so
    /// the second pass, with its fresh system-wide handle snapshot, ran on
    /// every whole-account scan): its security refuses a second open the
    /// same way ([`super::settled`]). A first pass whose only failures are
    /// refusals is the only pass; one with a read that failed after a granted
    /// open re-reads that process alone. NEGATIVE CONTROL: a refusing process
    /// is still held, named by the image its limited open read, or by none.
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

    /// A HANDLE CLOSED SINCE THE SNAPSHOT IS NO USE, NOT AN UNREADABLE READ
    /// (wave 7's live run, F1: a busy build or test process closed handles
    /// between the snapshot and their duplicate, and each one held its
    /// process, and with it every candidate, as unreadable): a parked child
    /// read with its snapshot's handles and a value it holds no handle of
    /// reads as using nothing. NEGATIVE CONTROL: read the same way against
    /// the folder it works in, it is still a user ("cwd"), so the closed
    /// value hides no use.
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
        // No process holds this many handles: the value names none of its.
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

    /// A child whose working directory is ELSEWHERE but which holds a file under
    /// the directory open uses it ("fd").
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

    /// A live child whose ONLY use of the directory is a held file whose path
    /// cannot be placed (a lone surrogate, a legal NTFS name no UTF-16 key
    /// spells) is held, never a non-user: S1 and S3. The control beside it
    /// proves the verdict is the file's: the same child holding a placeable file
    /// elsewhere reads as neither a user nor unreadable, so an ordinary process
    /// is not held for the handles every process has that will not duplicate.
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

    /// A module mapped from under the directory — the proc-macro DLL case,
    /// whose file handle the loader closes once it is mapped — makes its process
    /// a user ("exe") through the module list alone: its image and working
    /// directory are elsewhere and it holds no handle there. The module is a
    /// renamed copy of a system DLL (a mapped module under the directory is the
    /// point), mapped without running any of its code.
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

    /// `link` made a directory junction to `target` (`cmd /C mklink /J`: no
    /// privilege, as a directory symbolic link would need). `mklink` takes
    /// plain spellings only.
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

    /// A module mapped THROUGH A JUNCTION from under the directory makes its
    /// process a user ("exe"): the loader records the path as it was asked
    /// for (`…\link\target\…`, which places outside the canonical
    /// directory), and the kernel's name of the file mapped there, resolved,
    /// is what places it. The child's image and working directory are
    /// elsewhere and it holds no handle there, so the module is the only
    /// witness. NEGATIVE CONTROL: scanned against a directory it does not use,
    /// the same child reads cleanly — neither a user nor unreadable — so
    /// placing every module by its mapped file holds nobody it should not.
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
        // The path through the junction, as a build tool that was handed it
        // would load it.
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

    /// The kernel's name of a mapped file is spelled through the mount point
    /// the table lists for its device, any case of the device's name: a
    /// drive letter's and a folder mount point's. NEGATIVE CONTROLS: a share's
    /// redirector, a device the table mounts nowhere, a `subst` letter (it
    /// names a folder, never a device), a Win32 path and a bare `\Device\`
    /// are not placed.
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
            read_only: false,
            container: Some("Volume{11111111-2222-4333-8444-555555555555}".to_owned()),
        });
        table.mounts.push(Mount {
            on: PathBuf::from(r"S:\"),
            from: r"\Device\HarddiskVolume6".to_owned(),
            fstype: aterm_sysprobe::volumes::SUBST.to_owned(),
            local: true,
            automounted: false,
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

    /// A process whose IMAGE lives under the directory uses it ("exe"): a copy
    /// of `cmd.exe` placed in the directory and run, its working directory
    /// elsewhere. A running image under the directory is the point, so an `.exe`
    /// fixture is sanctioned here.
    #[test]
    fn a_running_image_under_the_directory_is_a_user() {
        let dir = fresh("image");
        let comspec = std::env::var_os("ComSpec").expect("ComSpec");
        let runner = dir.join("runner.exe");
        std::fs::copy(&comspec, &runner).expect("copy cmd.exe into the directory");
        // `/k` keeps it running, reading the stdin this process holds; its
        // working directory is the system temp, not the directory.
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

    /// A Git Bash (MSYS) child working under the directory is read through its
    /// PEB working directory, like any native process (measured: an MSYS
    /// process's `CurrentDirectory.DosPath` is accurate). Skipped, with a
    /// printed reason, where Git for Windows is not installed.
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

    /// A 32-bit (WOW64) process is read through its 32-bit PEB: a
    /// `SysWOW64\cmd.exe` started elsewhere that then changes its directory to
    /// one under the directory uses it ("cwd"). The change is made by the
    /// 32-bit code, so only the 32-bit process parameters carry it. Skipped,
    /// with a printed reason, where Windows has no 32-bit subsystem.
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

    /// A child blocked in a synchronous `ReadDirectoryChangesW` (a watcher)
    /// keeps its directory handle's lock for as long as it waits, and asking
    /// that handle's path waits behind it. The scan still returns (the hang
    /// detector), and reads the child as held: it is unreadable, never a
    /// non-user. The stuck query is remembered, so the next scan holds the
    /// child without waiting on it again; once the child ends, the abandoned
    /// helper's query returns and it forgets it. The product's own scan,
    /// under the product's deadline: that deadline firing is what is tested,
    /// and only what it made of the child is asserted, never how long it
    /// took.
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
            // The scan runs on a thread of its own, so one that hangs fails
            // here (the hang detector) instead of stalling the suite.
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
        // Between its ready file and its call the child is not blocked yet:
        // scan until the call holds the lock.
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

    /// `exe_of` names a live process's image, and reads a process that has
    /// exited as exited even while a handle to it (this process's, through the
    /// unreaped child) keeps its pid and its image name, as a zombie does.
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

    /// The verbatim, plain and differently-cased spellings of a kernel path all
    /// place under the one canonical directory (the S1 identity rule); a
    /// sibling whose name merely shares a prefix does not, nor does a path on
    /// another local volume of the table; a trailing separator does not change
    /// the answer. A share places the same way under itself — but a share
    /// path NOT under the directory is unknown, never "not under": only its
    /// server could say it is not the local disk again (`\\localhost\C$`), so
    /// it holds its process.
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
        // A `/` after a verbatim prefix is a literal character: unplaceable.
        assert_eq!(placed(Path::new(r"\\?\C:/Users//x/Proj"), &dir), None);
        // A drive root holds everything on its volume.
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
        // A share path against a local directory: unknown too.
        assert_eq!(placed(Path::new(r"\\srv\share\Proj\t"), &dir), None);
        // NEGATIVE CONTROL: a drive letter the table does not list (a `subst`
        // made since it was read, a letter mapped to a share) is unknown, not
        // "another volume".
        assert_eq!(placed(Path::new(r"S:\Users\x\Proj\target"), &dir), None);
        // A table not read whole places only what lies lexically under.
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

    /// A path rooted at neither a drive nor a share (the GLOBALROOT namespace,
    /// a device, a volume GUID the table does not list, a relative or
    /// drive-relative path), an 8.3 short name that names no file (so nothing
    /// spells it long), a verbatim path with an empty, `.` or `..` name (NTFS
    /// refuses them), or a Win32 name ending in a dot or a space (Win32 trims
    /// those), cannot be placed: `None`, which holds its process, never "not
    /// under". So does any path against a directory that cannot itself be
    /// keyed.
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
        // A short name in an empty folder names no file: held, though the
        // path, spelled long, would lie under the folder.
        let empty = fresh("short-none");
        let ghost = empty.join("NOSUCH~1").join("t");
        assert_eq!(
            placed(&ghost, std::slice::from_ref(&empty)),
            None,
            "{ghost:?}"
        );
        let _ = std::fs::remove_dir_all(&empty);
        // In a verbatim path a trailing dot is part of the name: another
        // directory.
        assert_eq!(
            placed(Path::new(r"\\?\C:\Users\x\Proj.\t"), &dir),
            Some(false)
        );
        let lone: Vec<u16> = r"\\?\C:\Users\x\P".encode_utf16().chain([0xD800]).collect();
        let unkeyable = PathBuf::from(std::ffi::OsString::from_wide(&lone));
        let path = Path::new(r"C:\Users\x\Proj\t");
        assert_eq!(placed(path, std::slice::from_ref(&unkeyable)), None);
        // A hit on a keyable directory still answers.
        assert_eq!(placed(path, &[unkeyable, dir[0].clone()]), Some(true));
    }

    /// A Win32 path places as Windows normalizes it: separator runs collapse,
    /// `.` drops and `..` removes the name before it, never the root. The
    /// loader keeps the spelling a process was created with, so a Git for
    /// Windows `bash.exe` reads as a module under `bin\..\usr\bin`.
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
        // `..` never climbs above the share, as Win32 resolves it.
        assert_eq!(
            placed(Path::new(r"\\srv\share\..\Proj"), &share),
            Some(true)
        );
        // Elsewhere on the share: unknown (only its server can say).
        assert_eq!(
            placed(Path::new(r"\\srv\share\..\other\Proj"), &share),
            None
        );
        // What the normalization makes of each: a verbatim path, a device
        // path and a drive-relative one come back as written.
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

    /// The scan's naming helper holds the system's error boxes off while it
    /// names (wave 3's soundness review, its first finding): its queries ask
    /// each file's own file system for its name, and one on an empty card
    /// reader or optical drive otherwise puts a box on the owner's desktop
    /// and parks the helper. Read ON the helper thread, through a probe
    /// query. NEGATIVE CONTROL: this test's own thread, which holds no guard,
    /// reads the boxes on.
    #[test]
    fn the_naming_helper_holds_the_error_boxes_off() {
        assert_eq!(helper_errors_quiet(HANG), Some(true), "the helper's thread");
        assert!(
            !aterm_sysprobe::errors_quiet(),
            "a thread with no guard has them on"
        );
    }

    /// A file object whose kernel name is a disk device itself (a volume, a
    /// whole disk or partition, an optical drive) holds no file; one with a
    /// path after its device, or on any other device, is not recognized so.
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

    /// A live child that refuses the scan's memory and handle reads but
    /// grants the image and liveness reads (as `ctfmon`, `taskhostw` and
    /// `DefenderSessionHelper` do on every Windows 11 session) is held, and
    /// named by its image — what a caller judging the unreadable needs. Its
    /// working directory is under the directory, but that cannot be read, so it
    /// is not a user: held is all the scan may say. `exe_of` names it too.
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

    /// A child that holds a volume open (`\\.\C:`, with no access, as any
    /// account may: search indexers and backup tools hold such handles) is
    /// read cleanly: the volume handle names no file (every name query fails
    /// on it, measured), and its kernel name says it is the volume itself.
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

    /// Case folds one UTF-16 unit to one, as NTFS's `$UpCase` does: `ß` is
    /// not `SS`, a letter with ypogegrammeni folds to its single simple
    /// uppercase, and a character outside the Basic Multilingual Plane is not
    /// folded at all.
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

    /// The full scan reads every process of this account, times itself, and
    /// names any same-user process it could not read. Printed for the
    /// campaign's measurement (run by hand with `--ignored --nocapture`): it
    /// opens every process of the account, the owner's window among them, for
    /// its memory and its handles, which no default test run may do.
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
