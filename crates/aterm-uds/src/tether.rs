// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A child that ends with the process that started it, a parent killed outright or crashed
//! included ([`spawnfd::Tether`](crate::spawnfd::Tether)).
//!
//! Linux has the parent-death signal: the child is spawned with `PR_SET_PDEATHSIG` set to
//! `SIGKILL`. Darwin has none, so a tethered spawn there runs this process's own executable as
//! [`VERB`] first, and that tether leaves a watcher behind as it execs the child in its own place:
//! the parent's handle names the child itself, its pid, its signals and its exit status. The
//! watcher is a grandchild of the tether, adopted by launchd, holding a kqueue `NOTE_EXIT` watch on
//! the parent and on the child. It `SIGKILL`s the child when the parent exits and ends when the
//! child does. The tether execs the child only once the watcher is armed and the parent is still
//! its own parent, so no order of deaths leaves the child running alone.

#[cfg(unix)]
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;

/// The verb a tethered spawn runs this executable with: `<exe> __tether <parent pid> <program>
/// <argv0> [args…]`.
pub const VERB: &str = "__tether";

/// A tether's exit code when it started nothing: the pid it was given is not its parent (that
/// parent died, or never was), or its watcher did not arm (`env`'s code for its own failure).
pub const UNTETHERED: u8 = 125;

/// A tether's exit code when the child's exec failed (the shell's).
pub const NOT_RUN: u8 = 127;

/// The executable this process's tethered spawns run through ([`entry`], [`through`]).
static THROUGH: OnceLock<Option<Through>> = OnceLock::new();

/// An executable recorded to run through, with its file as it was recorded.
struct Through {
    /// Its path.
    exe: PathBuf,
    /// Its file's device and inode then; `None` when they did not read.
    file: Option<(u64, u64)>,
}

impl Through {
    /// `exe`, with the file at its path now.
    #[cfg(any(unix, test, debug_assertions))]
    fn of(exe: PathBuf) -> Self {
        let file = file_at(&exe);
        Self { exe, file }
    }

    /// Its path, while the file there is still the one recorded.
    fn unchanged(&self) -> Option<&Path> {
        (self.file.is_some() && file_at(&self.exe) == self.file).then_some(self.exe.as_path())
    }
}

/// The device and inode of the file at `path`.
fn file_at(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// An executable's first call in `main`. Asked for [`VERB`], it serves it and answers the exit
/// code. Otherwise it records this executable as the one this process's tethered spawns run
/// through on Darwin, and answers `None`.
#[must_use]
pub fn entry() -> Option<ExitCode> {
    #[cfg(unix)]
    {
        let mut args = std::env::args_os().skip(1);
        if args.next().is_some_and(|first| first == VERB) {
            return Some(serve(&args.collect::<Vec<_>>()));
        }
        let _ = THROUGH.get_or_init(|| std::env::current_exe().ok().map(Through::of));
    }
    None
}

/// The executable a tethered spawn runs through: this process's own once its `main` called
/// [`entry`], while the file at its path is still the one recorded then. `None` in a process
/// whose `main` never did (a test binary's), and once an update or a rebuild put another file at
/// that path, which may be a build from before [`VERB`] that reads the tether's words as a
/// window's command line: tethered spawns then start the child untethered on Darwin.
#[must_use]
pub fn executable() -> Option<&'static Path> {
    THROUGH.get()?.as_ref()?.unchanged()
}

/// TEST SEAM: this process's tethered spawns run through `exe`, an executable whose `main` calls
/// [`entry`] first, for a test binary whose own does not. The first record stands.
#[cfg(any(test, debug_assertions))]
pub fn through(exe: PathBuf) {
    let _ = THROUGH.set(Some(Through::of(exe)));
}

/// The tether's argv for `program` started from this process: [`VERB`], this pid, the program,
/// its `argv[0]`, its arguments. `None` when this process records no executable to run through.
///
/// # Errors
/// An absolute `program` the exec would refuse, with the exec's own error (`ENOENT`, `EACCES`),
/// so the spawn fails as an untethered one would rather than the tether exiting [`NOT_RUN`].
#[cfg(target_vendor = "apple")]
pub(crate) fn route(
    program: &std::ffi::OsStr,
    arg0: Option<&std::ffi::OsStr>,
    args: &[OsString],
) -> std::io::Result<Option<(PathBuf, Vec<OsString>)>> {
    let Some(exe) = executable() else {
        return Ok(None);
    };
    if Path::new(program).is_absolute() {
        runnable(program)?;
    }
    let mut routed = Vec::with_capacity(args.len() + 4);
    routed.push(OsString::from(VERB));
    routed.push(OsString::from(std::process::id().to_string()));
    routed.push(program.to_owned());
    routed.push(arg0.unwrap_or(program).to_owned());
    routed.extend(args.iter().cloned());
    Ok(Some((exe.to_path_buf(), routed)))
}

/// Whether the exec could run `program` (an absolute path): `access(X_OK)`, and not a directory.
#[cfg(target_vendor = "apple")]
fn runnable(program: &std::ffi::OsStr) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(program.as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "nul byte found"))?;
    // SAFETY: a NUL-terminated path; `access` reads it and retains nothing.
    if unsafe { libc::access(path.as_ptr(), libc::X_OK) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if std::fs::metadata(program)?.is_dir() {
        return Err(std::io::Error::from_raw_os_error(libc::EACCES));
    }
    Ok(())
}

/// [`VERB`]: tie this process to `parent`, then exec the program in its place.
#[cfg(unix)]
fn serve(rest: &[OsString]) -> ExitCode {
    let [parent, program, argv0, args @ ..] = rest else {
        eprintln!("aterm: {VERB} <parent pid> <program> <argv0> [args…]");
        return ExitCode::from(UNTETHERED);
    };
    let Some(parent) = parent
        .to_str()
        .and_then(|pid| pid.parse::<i32>().ok())
        .filter(|&pid| pid > 1)
    else {
        eprintln!("aterm: {VERB}: {parent:?} is not a parent pid");
        return ExitCode::from(UNTETHERED);
    };
    if let Err(why) = hold(parent) {
        eprintln!(
            "aterm: {} was not started: {why}",
            Path::new(program).display()
        );
        return ExitCode::from(UNTETHERED);
    }
    let error = crate::spawnfd::command(program)
        .arg0(argv0)
        .args(args)
        .exec();
    eprintln!("aterm: {}: {error}", Path::new(program).display());
    ExitCode::from(NOT_RUN)
}

/// Words for a pid that is not this process's parent: that parent died, or never was.
#[cfg(unix)]
fn not_parent(parent: i32) -> String {
    format!("pid {parent} is not its parent")
}

/// Whether `parent` is still this process's parent.
#[cfg(unix)]
fn parent_is(parent: i32) -> bool {
    // SAFETY: `getppid` takes nothing and cannot fail.
    unsafe { libc::getppid() == parent }
}

/// Tie this process to `parent` before its exec: Linux's parent-death signal, which the exec
/// keeps.
#[cfg(target_os = "linux")]
fn hold(parent: i32) -> Result<(), String> {
    bind(parent).map_err(|e| match e.raw_os_error() {
        Some(libc::ESRCH) => not_parent(parent),
        _ => format!("PR_SET_PDEATHSIG: {e}"),
    })
}

/// No death tie here: only a check that `parent` is still this process's parent.
#[cfg(all(unix, not(target_os = "linux"), not(target_vendor = "apple")))]
fn hold(parent: i32) -> Result<(), String> {
    if parent_is(parent) {
        Ok(())
    } else {
        Err(not_parent(parent))
    }
}

/// `PR_SET_PDEATHSIG` to `SIGKILL`, then the check that `parent` did not die before it was set:
/// the kernel signals when the THREAD that forked this process exits. Async-signal-safe (two
/// system calls), for a spawn's hook between fork and exec.
///
/// # Errors
/// `prctl`'s, or `ESRCH` when `parent` is no longer this process's parent.
#[cfg(target_os = "linux")]
pub(crate) fn bind(parent: i32) -> std::io::Result<()> {
    const PR_SET_PDEATHSIG: i32 = 1;
    unsafe extern "C" {
        fn prctl(option: i32, ...) -> i32;
    }
    // SAFETY: `PR_SET_PDEATHSIG` takes one integer, the signal; it touches no memory of ours.
    if unsafe {
        prctl(
            PR_SET_PDEATHSIG,
            libc::c_ulong::from(libc::SIGKILL.unsigned_abs()),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    if parent_is(parent) {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(libc::ESRCH))
    }
}

/// Leave the watcher behind: fork, and fork again so launchd adopts it (the child this process
/// becomes has no child of its own to wait for); wait for it to arm, then check `parent` is still
/// this process's parent, so the watch it armed is on the right process.
#[cfg(target_vendor = "apple")]
fn hold(parent: i32) -> Result<(), String> {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
    if !parent_is(parent) {
        return Err(not_parent(parent));
    }
    let null = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .map_err(|e| format!("/dev/null: {e}"))?;
    let (mut armed, arming) = std::io::pipe().map_err(|e| format!("a pipe: {e}"))?;
    // SAFETY: `getpid` takes nothing and cannot fail.
    let child = unsafe { libc::getpid() };
    // SAFETY: `fork` takes nothing. This process has run nothing but `main`'s first call, so it
    // has one thread; the forked branch makes raw system calls only and never returns.
    let first = unsafe { libc::fork() };
    if first < 0 {
        return Err(format!("fork: {}", std::io::Error::last_os_error()));
    }
    if first == 0 {
        // SAFETY: the forked child, as above: `fork`, then the watcher or `_exit`.
        unsafe {
            let second = libc::fork();
            if second == 0 {
                watch(
                    parent,
                    child,
                    null.as_raw_fd(),
                    arming.as_raw_fd(),
                    armed.as_raw_fd(),
                );
            }
            libc::_exit(i32::from(second < 0));
        }
    }
    let mut status = 0;
    // SAFETY: `first` is this process's own child; `status` is a live local.
    while unsafe { libc::waitpid(first, &mut status, 0) } < 0
        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
    {}
    drop(arming);
    drop(null);
    let mut byte = [0_u8; 1];
    let said = loop {
        match armed.read(&mut byte) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            said => break said,
        }
    };
    if !parent_is(parent) {
        return Err(not_parent(parent));
    }
    match said {
        Ok(1) => Ok(()),
        _ => Err("its watcher did not arm".to_string()),
    }
}

/// One `EVFILT_PROC` `NOTE_EXIT` change for `pid`, with a receipt.
#[cfg(target_vendor = "apple")]
const fn exit_of(pid: i32) -> libc::kevent {
    libc::kevent {
        ident: pid.unsigned_abs() as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_RECEIPT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// Whether `events` hold `pid`'s exit.
#[cfg(target_vendor = "apple")]
fn exited(events: &[libc::kevent], pid: i32) -> bool {
    events
        .iter()
        .any(|e| e.ident == pid.unsigned_abs() as usize && e.fflags & libc::NOTE_EXIT != 0)
}

/// The watcher: stdio on `/dev/null`, a kqueue exit watch on `parent` and on `child`, the arming
/// byte, then a wait. `child`'s exit ends it; `parent`'s `SIGKILL`s `child` first, unless
/// `child`'s exit is queued already.
///
/// # Safety
/// Only in a forked child of the tether, with `null`, `arming` and `armed` open: raw system calls
/// only, on stack memory, as after any fork.
#[cfg(target_vendor = "apple")]
unsafe fn watch(parent: i32, child: i32, null: i32, arming: i32, armed: i32) -> ! {
    const BLANK: libc::kevent = exit_of(0);
    // SAFETY (whole body): the function's contract.
    unsafe {
        libc::close(armed);
        for fd in 0..3 {
            libc::dup2(null, fd);
        }
        if null > 2 {
            libc::close(null);
        }
        let kq = libc::kqueue();
        if kq < 0 {
            libc::_exit(1);
        }
        let changes = [exit_of(parent), exit_of(child)];
        let mut receipts = [BLANK; 2];
        let got = libc::kevent(
            kq,
            changes.as_ptr(),
            2,
            receipts.as_mut_ptr(),
            2,
            std::ptr::null(),
        );
        if got != 2
            || receipts
                .iter()
                .any(|r| r.flags & libc::EV_ERROR != 0 && r.data != 0)
        {
            libc::_exit(1);
        }
        if libc::write(arming, [1_u8].as_ptr().cast(), 1) != 1 {
            libc::_exit(1);
        }
        libc::close(arming);
        let now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        loop {
            let mut events = [BLANK; 2];
            let got = libc::kevent(
                kq,
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                2,
                std::ptr::null(),
            );
            let Ok(got) = usize::try_from(got) else {
                if *libc::__error() == libc::EINTR {
                    continue;
                }
                libc::_exit(1);
            };
            let events = &events[..got.min(2)];
            if exited(events, child) {
                libc::_exit(0);
            }
            if exited(events, parent) {
                let mut queued = [BLANK; 2];
                let more = libc::kevent(kq, std::ptr::null(), 0, queued.as_mut_ptr(), 2, &now);
                let more = usize::try_from(more).unwrap_or(0).min(2);
                if !exited(&queued[..more], child) {
                    libc::kill(child, libc::SIGKILL);
                }
                libc::_exit(0);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::Through;

    #[test]
    fn a_file_put_in_place_of_the_recorded_executable_is_not_run_through() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let exe = dir.path().join("aterm");
        std::fs::write(&exe, b"the build this process started from").expect("write");
        let through = Through::of(exe.clone());
        assert_eq!(
            through.unchanged(),
            Some(exe.as_path()),
            "the recorded file"
        );
        let next = dir.path().join("aterm.next");
        std::fs::write(&next, b"another build").expect("write");
        std::fs::rename(&next, &exe).expect("rename over");
        assert_eq!(through.unchanged(), None, "another file at its path");
        std::fs::remove_file(&exe).expect("remove");
        assert_eq!(through.unchanged(), None, "no file at its path");
        assert_eq!(
            Through::of(exe.clone()).unchanged(),
            None,
            "nothing recorded"
        );
    }
}
