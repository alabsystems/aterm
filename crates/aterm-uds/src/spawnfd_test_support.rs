// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only raw spawns for descriptor-inheritance tests (`#[cfg(test)]`).

use super::{InheritStrip, pass_libnotify_gate, waitpid};
/// Every crate's shell probe of a descriptor, which this crate's tests call from here.
pub(crate) use crate::host::{held_probe, sh_held, sh_held_any};
use core::ffi::c_char;
use std::ffi::{CString, OsStr};
use std::io::{self, Read as _};
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::time::Duration;

/// What [`leaky_output`] gives back: the status and the captured stdout.
pub(crate) struct LeakyOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
}

/// Run `program` (an absolute path) with `args`, DELIBERATELY LEAKY.
/// # Errors
/// A NUL in the program or an argument, or the spawn's own failure.
pub(crate) fn leaky_status(program: &str, args: &[impl AsRef<OsStr>]) -> io::Result<ExitStatus> {
    let image = Image::new(program, args)?;
    reap(leaky_spawn(&image, None)?)
}

/// [`leaky_status`] with the child's stdout captured and its stderr on `/dev/null`.
/// # Errors
/// As [`leaky_status`], or a failed read of the captured stdout.
pub(crate) fn leaky_output(program: &str, args: &[impl AsRef<OsStr>]) -> io::Result<LeakyOutput> {
    let image = Image::new(program, args)?;
    let (mut near, far) = UnixStream::pair()?;
    bound_read(&near)?;
    let null = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
    let pid = leaky_spawn(&image, Some((far.as_raw_fd(), null.as_raw_fd())))?;
    drop(far);
    drop(null);
    let mut stdout = Vec::new();
    let read = near.read_to_end(&mut stdout);
    let status = reap(pid)?;
    read?;
    Ok(LeakyOutput { status, stdout })
}

/// Run `program` (an absolute path) with `args` through `fork`, [`InheritStrip::apply`], and `execve`.
/// # Errors
/// A NUL in the program or an argument, a failed `fork`, or the errno of a failed `execve`.
pub(crate) fn stripped_status(program: &str, args: &[impl AsRef<OsStr>]) -> io::Result<ExitStatus> {
    let image = Image::new(program, args)?;
    reap(fork_exec(&image, None, Some(InheritStrip::prepare()))?)
}

struct Image {
    program: CString,
    argv: Vec<CString>,
    envp: Vec<CString>,
}

impl Image {
    fn new(program: &str, args: &[impl AsRef<OsStr>]) -> io::Result<Self> {
        assert!(
            program.starts_with('/'),
            "a leaky spawn takes an absolute path, not {program:?}: it does no PATH search"
        );
        let c = |bytes: &[u8]| {
            CString::new(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL"))
        };
        let program = c(program.as_bytes())?;
        let mut argv = vec![program.clone()];
        for arg in args {
            argv.push(c(arg.as_ref().as_bytes())?);
        }
        let mut envp = Vec::new();
        for (k, v) in std::env::vars_os() {
            let mut kv = k.as_bytes().to_vec();
            kv.push(b'=');
            kv.extend_from_slice(v.as_bytes());
            envp.push(c(&kv)?);
        }
        Ok(Self {
            program,
            argv,
            envp,
        })
    }

    fn pointers(strings: &[CString]) -> Vec<*const c_char> {
        strings
            .iter()
            .map(|s| s.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect()
    }
}

/// Put a 60 s read timeout.
pub(crate) fn bound_read(near: &UnixStream) -> io::Result<()> {
    near.set_read_timeout(Some(Duration::from_secs(60)))
}

/// Run `round` while a background thread repeatedly stats `/dev/fd/{number}` until
/// [`Looker::probe`] observes `races` `EBADF` races (Darwin's `/dev/fd/N` shares a single vnode
/// across lookups).
#[cfg(test)]
#[cfg(target_vendor = "apple")]
#[track_caller]
pub(crate) fn racing_lookups(number: RawFd, races: usize, mut round: impl FnMut(&Looker)) -> usize {
    use std::sync::atomic::Ordering;
    super::tests::require_soft_limit(u64::try_from(number).unwrap_or(0) + 1, number);
    let path = format!("/dev/fd/{number}");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread = {
        let (stop, path) = (std::sync::Arc::clone(&stop), path.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = std::fs::metadata(&path);
            }
        })
    };
    let looker = Looker {
        path,
        raced: std::cell::Cell::new(0),
    };
    let hang = std::time::Instant::now() + Duration::from_secs(60);
    let mut rounds = 0;
    while looker.raced.get() < races && std::time::Instant::now() < hang {
        round(&looker);
        rounds += 1;
    }
    stop.store(true, Ordering::Relaxed);
    thread.join().expect("the looking thread");
    let raced = looker.raced.get();
    if raced < races {
        crate::host::could_not_run(format!(
            "two lookups of {} never raced in a minute ({raced} of {races} in {rounds} rounds)",
            looker.path
        ));
    }
    rounds
}

/// [`racing_lookups`]' control: a path `stat` of the contended number.
#[cfg(target_vendor = "apple")]
pub(crate) struct Looker {
    path: String,
    raced: std::cell::Cell<usize>,
}

#[cfg(target_vendor = "apple")]
impl Looker {
    /// `stat` the contended number by its `/dev/fd` path, which the caller holds open now, counting
    /// a failure as a race.
    pub(crate) fn probe(&self) {
        if std::fs::metadata(&self.path).is_err() {
            self.raced.set(self.raced.get() + 1);
        }
    }
}

fn reap(pid: i32) -> io::Result<ExitStatus> {
    let mut raw = 0;
    loop {
        // SAFETY: our own child; `raw` is a live local.
        if unsafe { waitpid(pid, &mut raw, 0) } == pid {
            return Ok(ExitStatus::from_raw(raw));
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// THE LEAKY SPAWN on Darwin: `posix_spawn` with NO attributes, so no
/// `POSIX_SPAWN_CLOEXEC_DEFAULT`, and file actions only to put `out`'s two
/// descriptors at 1 and 2. Every descriptor this process holds without
/// close-on-exec at the spawn reaches the child, as it did through upstream
/// std's `Command`.
#[cfg(target_vendor = "apple")]
fn leaky_spawn(image: &Image, out: Option<(RawFd, RawFd)>) -> io::Result<i32> {
    type Attr = *mut core::ffi::c_void;
    type Actions = *mut core::ffi::c_void;
    unsafe extern "C" {
        fn posix_spawn_file_actions_init(actions: *mut Actions) -> i32;
        fn posix_spawn_file_actions_destroy(actions: *mut Actions) -> i32;
        fn posix_spawn_file_actions_adddup2(actions: *mut Actions, fd: i32, new: i32) -> i32;
        fn posix_spawn(
            pid: *mut i32,
            path: *const c_char,
            actions: *const Actions,
            attr: *const Attr,
            argv: *const *const c_char,
            envp: *const *const c_char,
        ) -> i32;
    }
    let argv = Image::pointers(&image.argv);
    let envp = Image::pointers(&image.envp);
    let mut actions: Actions = std::ptr::null_mut();
    // SAFETY: init writes a fresh file-actions object into `actions`.
    let rc = unsafe { posix_spawn_file_actions_init(&mut actions) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    let mut pid = 0;
    // SAFETY: `actions` was initialised above and is destroyed once, below; the program, `argv` and
    // `envp` are C strings and NULL-terminated arrays of them that outlive the call. A NULL
    // attribute pointer asks posix_spawn for its documented default attributes.
    let rc = unsafe {
        let mut rc = 0;
        if let Some((stdout, stderr)) = out {
            rc = posix_spawn_file_actions_adddup2(&mut actions, stdout, 1);
            if rc == 0 {
                rc = posix_spawn_file_actions_adddup2(&mut actions, stderr, 2);
            }
        }
        if rc == 0 {
            rc = posix_spawn(
                &mut pid,
                image.program.as_ptr(),
                &actions,
                std::ptr::null(),
                argv.as_ptr(),
                envp.as_ptr(),
            );
        }
        posix_spawn_file_actions_destroy(&mut actions);
        rc
    };
    if rc == 0 {
        Ok(pid)
    } else {
        Err(io::Error::from_raw_os_error(rc))
    }
}

/// THE LEAKY SPAWN off Darwin: `fork` and `execve` with no strip.
#[cfg(not(target_vendor = "apple"))]
fn leaky_spawn(image: &Image, out: Option<(RawFd, RawFd)>) -> io::Result<i32> {
    fork_exec(image, out, None)
}

unsafe extern "C" {
    fn fork() -> i32;
    fn execve(path: *const c_char, argv: *const *const c_char, envp: *const *const c_char) -> i32;
    fn dup2(old: RawFd, new: RawFd) -> RawFd;
    fn write(fd: RawFd, buf: *const core::ffi::c_void, count: usize) -> isize;
    fn _exit(code: i32) -> !;
}

fn fork_exec(
    image: &Image,
    out: Option<(RawFd, RawFd)>,
    strip: Option<InheritStrip>,
) -> io::Result<i32> {
    let argv = Image::pointers(&image.argv);
    let envp = Image::pointers(&image.envp);
    let (mut status_rd, status_wr) = UnixStream::pair()?;
    bound_read(&status_rd)?;
    let report = status_wr.as_raw_fd();
    pass_libnotify_gate();
    // SAFETY: `fork` takes no arguments; the child branch never returns.
    let pid = unsafe { fork() };
    if pid == 0 {
        // SAFETY: the forked child. Async-signal-safe calls only (the strip's raw syscalls, `dup2`,
        // `execve`, `write`, `_exit`), on the stack and on memory this process prepared before the
        // fork; nothing is heap-allocated.
        unsafe {
            if let Some(strip) = strip {
                strip.apply();
            }
            if let Some((stdout, stderr)) = out
                && (dup2(stdout, 1) < 0 || dup2(stderr, 2) < 0)
            {
                fail(report);
            }
            execve(image.program.as_ptr(), argv.as_ptr(), envp.as_ptr());
            fail(report)
        }
    }
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    drop(status_wr);
    let mut word = Vec::new();
    status_rd.read_to_end(&mut word)?;
    if word.is_empty() {
        return Ok(pid);
    }
    reap(pid)?;
    let errno = <[u8; 4]>::try_from(word.as_slice())
        .map_err(|_| io::Error::other("the child's exec-status report was cut short"))?;
    Err(io::Error::from_raw_os_error(i32::from_be_bytes(errno)))
}

/// Report this child's errno on the status pipe and exit.
/// # Safety
/// Only in the forked child.
unsafe fn fail(report: RawFd) -> ! {
    let word = io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(0)
        .to_be_bytes();
    // SAFETY: `write` and `_exit` are async-signal-safe; `word` is live.
    unsafe {
        write(report, word.as_ptr().cast(), word.len());
        _exit(127)
    }
}
