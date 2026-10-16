// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PATH-USER SCAN: which live processes of this uid are USING a path under
//! one of a set of directories — as their working directory, as their
//! executable, or through an open descriptor.
//!
//! The disk watch removes a build directory WHOLE only when its owner is gone
//! (`aterm_agent::harness::disk`). Cargo takes no lock while tests run, and a
//! binary run from `deps/` holds none either, so "the owner is gone" is
//! checked against what is RUNNING too: a process whose working directory,
//! executable or open file lies under the directory (or under the owner's own
//! folder) keeps it. Measured 2026-10-01 by the review's probe: a process
//! with its working directory in `target/debug/deps` and an `.rlib` held open
//! saw that directory deleted before this scan existed.
//!
//! macOS reads it through libproc, as the rdev scan beside it does: per
//! process `proc_pidinfo(PROC_PIDVNODEPATHINFO)` (the working directory),
//! `proc_pidpath` (the executable) and, per vnode descriptor,
//! `proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO)`. Linux reads `/proc/<pid>/cwd`,
//! `exe` and `fd/*`. A process that EXITED holds nothing (`ESRCH`, or its
//! `/proc` entry gone), whether it exited before the listing named it or
//! between its reads; a descriptor that closed mid-read is gone, and so is
//! one on a DEAD vnode — revoked, its read answering `ENOENT` (the holder
//! scan's module docs) — which names no path; a process with no descriptor
//! at all is read.
//!
//! So is a process whose WORKING DIRECTORY is a dead vnode: it works in no
//! path, so in none under the directories, and its executable and
//! descriptors still say what it uses. In xnu (xnu-12377.121.6,
//! `bsd/kern/proc_info.c`), `PROC_PIDVNODEPATHINFO` (`proc_pidvnodepathinfo`)
//! takes the working directory's vnode with `vnode_getwithvid`, as a
//! descriptor's read takes its vnode, and answers its `ENOENT` — the vnode
//! reclaimed (`VL_DEAD`) or its id moved because it was — before reading
//! anything else. A directory is never revoked (`revoke(2)` takes devices
//! alone, `ENOTSUP` for anything else), so a working directory dies with its
//! volume: a forced unmount (`hdiutil detach -force`, `diskutil unmount
//! force`, a disk pulled out). Measured 2026-10-01: a child working at the
//! root of a disk image read `ENOENT` for good once the image was
//! force-detached; read as unknown, such a process kept every gone owner's
//! build directory from the disk watch for its whole life
//! (`aterm_agent::harness::disk_discover::unused`). After the vnode is
//! taken, a refusal — the MAC and same-uid checks, the filesystem's getattr
//! (`fill_vnodeinfo`) — leaves the process unreadable, and the one `ENOENT`
//! a getattr can give is a filesystem saying the directory no longer
//! exists, which names no path either. The same `ENOENT` answers a dead
//! ROOT directory, read after the working directory: a root is set by
//! `chroot(2)` alone, which xnu grants the superuser alone (a fork inherits
//! it), so the one process read as working nowhere when it is not is one the
//! superuser confined to a chroot, working outside it on another volume,
//! after the chroot's own volume was force-unmounted. An `ENOENT` listing a
//! table stays unknown.
//!
//! Linux has no such failure to tell apart: a hung-up terminal (`vhangup`,
//! its controlling process's exit) keeps its path, which `/proc/<pid>/fd/<n>`
//! reads like any other (`__tty_hangup` swaps only the file's operations),
//! and a deleted target, a working directory among them, reads as its path
//! and ` (deleted)`. A process whose answers could not be read otherwise is
//! [`PathScan::unreadable`], never a non-user: the caller decides what that
//! means. Paths are compared as the kernel writes them, with the directories
//! given CANONICAL; a kernel path under `/System/Volumes/Data` (the data
//! volume's firmlinked form) is compared without that prefix too.
//! Memory-mapped files that hold no descriptor are not seen.
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
    /// Processes scanned (by the full scan, this uid's) whose working
    /// directory, executable or descriptors could not be read (not because
    /// they exited), each with its executable when that much could be read.
    /// Measured 2026-10-01 on macOS 26: 8 of 335 — `lsd`, `UserEventAgent`,
    /// `routined`, `nsurlsessiond` and other platform agents refuse the
    /// working directory or the descriptor list to a same-uid reader.
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
/// the parent's.
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

/// [`scan_path_users`] over the processes `pids` alone, each read as that
/// scan reads it — the process universe, NAMED: the full scan is this over
/// every process of the uid, and a test names the processes it started, so
/// what else the machine runs (a process another session starts, one that
/// cannot be read) never reaches its verdict. A pid no process has is one
/// that exited: it uses nothing. Every pid named is read, whatever its uid,
/// on both platforms — the uid is the full scan's LISTING, never this
/// reader's filter — so another uid's process is read like any other, and
/// is mostly [`PathScan::unreadable`]: the kernel refuses its reads.
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
/// executable (a fork that has not exec'd)?
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn unexeced_fork(pid: u32, parents: &[u32]) -> bool {
    let Some(ppid) = parent_of(pid) else {
        return false;
    };
    parents.contains(&ppid)
        && match (executable(pid), executable(ppid)) {
            (Some(child), Some(parent)) => child == parent,
            _ => false,
        }
}

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
    /// It RUNS, and what it runs has no path to name: unlinked since its
    /// exec (a rebuilt binary, a replaced app), or the read refused.
    Unnamed,
    /// It has exited (`ESRCH`, which a zombie answers too; on Linux, its
    /// `exe` gone): it runs nothing.
    Exited,
}

/// The executable of `pid`, as the kernel holds it — told apart from a
/// process that has EXITED, which is not one whose executable cannot be
/// named: that one still runs.
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
#[cfg(target_os = "linux")]
#[must_use]
pub fn exe_of(pid: u32) -> Exe {
    match std::fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(path) => Exe::At(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Exe::Exited,
        Err(_) => Exe::Unnamed,
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
            // An executable unlinked since the exec has no path left to report,
            // which is no failure to read the process: its working directory and
            // descriptors still say what it uses. Counted unreadable, one such
            // process (a debug build whose worktree was deleted, 2026-10-01) kept
            // every build directory on the machine from being reclaimed.
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

#[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
mod tests {
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

    /// A CHILD WHOSE WORKING DIRECTORY IS UNDER THE DIRECTORY USES IT, and
    /// one that holds a file there open does too; NEGATIVE CONTROL, a
    /// directory nobody is in has no user, and this process is skipped.
    #[test]
    fn a_child_working_or_holding_a_file_under_the_directory_uses_it() {
        let dir = fresh("cwd");
        let deep = dir.join("debug/deps");
        std::fs::create_dir_all(&deep).expect("deps");
        // `spawnfd`'s, every child of these scans: each holds its stdio and
        // nothing else of this process, not another test thread's pipe for as
        // long as it lives.
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

    /// A PROCESS WHOSE EXECUTABLE WAS DELETED IS STILL READ: `proc_pidpath`
    /// answers `ENOENT` for it, and its working directory under the directory
    /// still makes it a user — never an unreadable process that rules nothing
    /// out. The copy of `cat` is signed ad hoc, or the kernel kills it, and is
    /// deleted only once the child has echoed a line back — its own fact that
    /// its launch is done. The kernel names the executable before the launch
    /// is done, and a fresh executable deleted before then gets its process
    /// killed (the system's scan of the file fails to open it): measured
    /// 2026-10-01, a copy deleted right after its spawn was killed 20 times
    /// of 20, one deleted after its echo 0 of 20. The child ends at its EOF,
    /// so a failed assertion leaves nothing running.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_process_whose_executable_was_deleted_is_read_through_its_working_directory() {
        use std::io::{BufRead as _, Write as _};
        // Re-signing a copy of an Apple binary waits on trustd.
        if crate::deadline::trust_service_silent() {
            return;
        }
        let dir = fresh("unlinked");
        let (bin, work) = (dir.join("catter"), dir.join("work"));
        std::fs::create_dir_all(&work).expect("work");
        std::fs::copy("/bin/cat", &bin).expect("copy cat");
        crate::deadline::sign_ad_hoc(&bin);
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

    /// A FORK OF THIS PROCESS THAT HAS NOT EXEC'D IS THIS PROCESS: it holds
    /// copies of every descriptor this process had at the fork (the disk
    /// watch's own handles among them), so it is skipped with it.
    /// NEGATIVE CONTROL: unskipped, the same fork reads as a user.
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
        assert!(
            !skipped.users.iter().any(|u| u.pid == child_pid),
            "{:?}",
            skipped.users
        );
        assert!(
            unskipped.users.iter().any(|u| u.pid == child_pid),
            "{:?}",
            unskipped.users
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A child's pid once the kernel shows it running `/bin/sleep`: its
    /// working directory and the descriptors it kept across the exec are
    /// its own from then on.
    #[cfg(target_vendor = "apple")]
    fn exec_done(child: &crate::spawnfd::Child) -> u32 {
        running(child, "/bin/sleep")
    }

    /// A child's pid once the kernel shows it running `exe` (see
    /// [`exec_done`]).
    #[cfg(target_vendor = "apple")]
    fn running(child: &crate::spawnfd::Child, exe: &str) -> u32 {
        let pid = child.id();
        let start = Instant::now();
        while super::executable(pid).as_deref() != Some(std::path::Path::new(exe)) {
            assert!(start.elapsed() < HANG, "the child never ran {exe}");
            std::thread::sleep(Duration::from_millis(5));
        }
        pid
    }
    /// A child with NO descriptor at all, working in `cwd`, STOPPED, and its
    /// pid once the stop is reported: a shell spawned with nothing but its
    /// stdio (`spawnfd::command`), which closes those three and stops itself,
    /// so nothing it runs can open one again. SIGKILL it and reap it with
    /// [`reap`].
    ///
    /// SPAWNED, NOT FORKED. A raw `fork` of this multithreaded test binary
    /// dies before its first line of code now and then: libSystem's atfork
    /// child handler (`_notify_fork_child`) meets an `os_once` another test's
    /// thread is inside at the fork, and aborts the child
    /// (`_os_once_gate_corruption_abort`, a SIGKILL and a crash report each
    /// time) — `stopped, not exited: 0x9`, 2 of 40 whole-binary runs.
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

    /// A DESCRIPTOR ON A REVOKED TERMINAL HOLDS NOTHING, and its process is
    /// still read. A child holds a PTY slave open; `revoke(2)` on the slave's
    /// path reclaims the vnode under the child's descriptor — what a
    /// terminal's controlling process does to it at its exit — and the
    /// descriptor's read answers `ENOENT` from then on. Scanned for the very
    /// path that descriptor named, the child is no user and not unreadable,
    /// and it holds no device: before, ONE such descriptor made its process
    /// unknown for its whole life, and the disk watch kept every gone
    /// owner's build directory while it ran. Before the revoke, both scans
    /// find it through that descriptor. NEGATIVE CONTROL: the same reads,
    /// classified as before a dead vnode was told apart, make it unknown to
    /// both scans.
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
        // The slave on descriptor 3, from a working directory and an
        // executable that are not it.
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

    /// `hdiutil`'s path.
    #[cfg(target_vendor = "apple")]
    const HDIUTIL: &str = "/usr/bin/hdiutil";

    /// `hdiutil detach -force <dev>`, waited for.
    #[cfg(target_vendor = "apple")]
    fn detach(dev: &str) -> std::process::Output {
        std::process::Command::new(HDIUTIL)
            .args(["detach", "-force", dev])
            .stdin(std::process::Stdio::null())
            .output()
            .expect("hdiutil detach")
    }

    /// A VOLUME OF THIS TEST'S: a fresh disk image's, attached at a mount
    /// point — no privilege needed, hidden from the Finder — and detached by
    /// force when it is dropped still attached, so a failed assertion leaves
    /// no mount behind.
    #[cfg(target_vendor = "apple")]
    struct Volume {
        /// Its device (`/dev/diskN`); empty once detached.
        dev: String,
    }

    #[cfg(target_vendor = "apple")]
    impl Volume {
        /// A 2 MB HFS+ image created at `image` and attached at `mnt`;
        /// `None`, after one line on stderr, when macOS's disk-image service
        /// did not answer (`crate::deadline::disk_image_service_silent`). Not
        /// `-quiet`: that would hide the `Operation timed out` the judgment reads.
        fn attach(image: &std::path::Path, mnt: &std::path::Path) -> Option<Self> {
            use std::process::Stdio;
            let created = std::process::Command::new(HDIUTIL)
                .args(["create", "-size", "2m", "-fs", "HFS+"])
                .args(["-layout", "NONE", "-volname", "aterm-uds-test"])
                .arg(image)
                .stdin(Stdio::null())
                .output()
                .expect("hdiutil create");
            if crate::deadline::disk_image_service_silent(&created) {
                return None;
            }
            assert!(created.status.success(), "hdiutil create: {created:?}");
            let attached = std::process::Command::new(HDIUTIL)
                .args(["attach", "-nobrowse", "-noverify", "-noautoopen"])
                .arg("-mountpoint")
                .arg(mnt)
                .arg(image)
                .stdin(Stdio::null())
                .output()
                .expect("hdiutil attach");
            if crate::deadline::disk_image_service_silent(&attached) {
                return None;
            }
            assert!(attached.status.success(), "hdiutil attach: {attached:?}");
            let out = String::from_utf8_lossy(&attached.stdout);
            let dev = out
                .split_whitespace()
                .find(|w| w.starts_with("/dev/disk"))
                .unwrap_or_else(|| panic!("no device in {out:?}"));
            Some(Self {
                dev: dev.to_owned(),
            })
        }

        /// Detach it by force — what pulling out a disk does to its volume:
        /// every vnode on it reclaimed under whatever still holds one.
        fn detach_by_force(&mut self) -> std::process::Output {
            let out = detach(&self.dev);
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

    /// A DEAD WORKING DIRECTORY IS NO WORKING DIRECTORY, and its process is
    /// still read. A child works at the root of a disk image's volume and
    /// holds a file elsewhere open; `hdiutil detach -force` unmounts the
    /// volume from under it — what pulling out a disk does — and the kernel
    /// reclaims the vnode its working directory is, whose read answers
    /// `ENOENT` from then on. Scanned for the volume's mount point, the child
    /// is then no user and not unreadable; scanned for the directory of the
    /// file it holds, it is a user through that descriptor — read on past
    /// the dead directory. Before the detach, it used the mount point as its
    /// working directory. NEGATIVE CONTROL: the same reads, classified as
    /// before a dead vnode was told apart, make it unreadable — and the disk
    /// watch kept every gone owner's build directory while it ran. No
    /// privilege is needed; the volume is detached even when an assertion
    /// fails, the mount point is its parent's device again after, and the
    /// child ends at its stdin's EOF.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_dead_working_directory_is_no_working_directory() {
        use crate::spawnfd::Stdio;
        use std::os::unix::fs::MetadataExt as _;
        let me = std::process::id();
        let base = fresh("deadcwd");
        let (image, mnt, held) = (base.join("v.dmg"), base.join("mnt"), base.join("held"));
        std::fs::create_dir_all(&mnt).expect("the mount point");
        std::fs::create_dir_all(&held).expect("the held file's directory");
        let file = held.join("open.rlib");
        std::fs::write(&file, b"x").expect("a file to hold");
        let device = |p: &std::path::Path| std::fs::metadata(p).expect("stat").dev();
        let Some(mut volume) = Volume::attach(&image, &mnt) else {
            return;
        };
        let mounted = device(&mnt) != device(&base);
        // At the volume's root, the file on descriptor 3; `cat` ends when this
        // test's end of its stdin closes, a panic's unwinding among the ways.
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
            super::scan_path_users_of(&[pid], std::slice::from_ref(dir), &[me]).expect("scan")
        };
        let before = scan(&mnt);
        let detached = volume.detach_by_force();
        let unmounted = device(&mnt) == device(&base);
        let read = super::mac::cwd(pid as i32);
        let after = scan(&mnt);
        let through_fd = scan(&held);
        let old = super::super::hook::reading_dead_as_unknown(|| scan(&mnt));
        let _ = child.kill();
        let _ = child.wait();
        drop(volume);

        assert!(mounted, "the image is mounted at {mnt:?}");
        assert_eq!(
            before.users,
            [super::PathUse {
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

    /// A PROCESS WITH NO DESCRIPTOR AT ALL IS READ, and holds nothing: libproc
    /// answers its empty table with 0 and no `errno` — an answer, not a
    /// refusal. A test harness's process-group anchor (a Python that
    /// `closerange`s every descriptor and waits, another session's) once read as
    /// unreadable, and its every appearance kept every gone owner's build
    /// directory on the machine. Its working directory is still
    /// read: from inside the directory it is a user. NEGATIVE CONTROL: a table
    /// the kernel refuses (launchd's, read because it is named, whatever its
    /// uid) is still unreadable.
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

    /// A PROCESS THAT EXITED AFTER A LISTING NAMED IT USES NOTHING, and is
    /// never unreadable: every read of it answers `ESRCH` — killed and not
    /// yet reaped (a zombie the listing still names), reaped, or killed
    /// between its descriptors' listing and their reads. Each was a user
    /// first: working in the directory, or holding a file of it open.
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
