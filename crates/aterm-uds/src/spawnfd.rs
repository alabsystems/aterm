// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! START A CHILD THAT INHERITS EXACTLY WHAT IT IS HANDED — two descriptors at
//! fixed numbers plus its stdio, or its stdio alone ([`command`]), and nothing
//! else; on Darwin without a fork — and, for the spawns that must still fork,
//! the strip that keeps everything else out.
//!
//! ## The two lanes
//!
//! aterm launches its `aterm-link serve` child holding the far ends of two
//! `socketpair(AF_UNIX)`s at **fds 3 and 4** (DESIGN-aterm-fabric.md §11.2). The
//! authority of that child is the DESCRIPTOR and nothing else — no token file
//! exists to steal, and the only way to be the bridge is to be the process aterm
//! spawned. `std::process::Command` can place an inherited descriptor at 0, 1 or
//! 2 and nowhere else, so [`spawn_with_two_fds`] takes a [`LaneCommand`] and
//! returns a [`LaneChild`] of its own.
//!
//! ## Why not a `Command` that forks and strips
//!
//! A `Command` whose `pre_exec` closure marks every number from 3 up to a
//! ceiling close-on-exec and then `dup2`s the lanes into place fails three
//! ways (the descriptor-protocol model, `trust-platform`, validated against
//! this Mac's kernel):
//!
//! 1. A CEILING CAPPED AT 65 536 MISSES DESCRIPTORS: Darwin allocates them up to
//!    `kern.maxfilesperproc` (92 160 measured) whenever the soft
//!    `RLIMIT_NOFILE` is higher — a shell's is 1 048 576 here, and aterm-gui
//!    raises its own. Every unflagged descriptor at 65 536..92 159 reaches the
//!    bridge, a process that lives as long as the window.
//! 2. A CEILING READ IN THE PARENT, before the fork, races: another thread that
//!    raises the limit and opens an unflagged descriptor above the old ceiling
//!    in that window hands it to the child.
//! 3. A `pre_exec` CLOSURE THAT `dup2`s ONTO 3 AND 4 CAN CLOBBER `std`'s OWN
//!    EXEC-STATUS PIPE, which `std` opens at the lowest free numbers just before
//!    its fork. With its write end replaced, a failed `exec` reads as EOF in the
//!    parent — `Ok(child)` for a child that already died — and the error bytes
//!    go down the lane instead.
//!
//! ## Darwin: `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`
//!
//! The kernel marks every descriptor close-on-exec inside the spawn except the
//! ones a file action names, so nothing can be added to the child and there is
//! no strip, no ceiling and no window. MEASURED facts this rests on (the model
//! work of 2026-09-30, Darwin 25):
//!
//! * only a descriptor that a file action names AND that is not close-on-exec
//!   at the exec survives;
//! * `adddup2(fd, fd)` keeps `FD_CLOEXEC` — so a source already sitting on its
//!   target would arrive CLOSED — and `addinherit_np(fd)` clears it;
//! * libc refuses a file action naming any number >= 10 240 (`OPEN_MAX`) with
//!   `EBADF` at add time;
//! * the table copy is not atomic, but a concurrent change can only DROP a slot
//!   from the child, never add one.
//!
//! So every source below 5 or at 10 240 and up is first moved, in the parent, to
//! a fresh `F_DUPFD_CLOEXEC` copy at 5 or above ([`off_the_targets`]): no
//! `dup2` is then a self-copy, none clobbers a source a later action reads, and
//! every number a file action names is one libc accepts. The copies are
//! close-on-exec from birth, so a concurrent `fork` elsewhere inherits none —
//! bar an EXEC's: `POSIX_SPAWN_SETEXEC` closes close-on-exec descriptors before
//! its file actions run, so an exec takes a flagged source from an unflagged
//! copy, held for the instant before this process is replaced. Call `exec`
//! from a process no other thread is spawning in.
//!
//! ## Linux: our own fork, `close_range`
//!
//! Linux has no `CLOEXEC_DEFAULT`, so the child is forked here — not through
//! `Command`, whose exec-status pipe is the clobbering hazard above. Our own
//! status pipe's write end sits at 5 or above like every source, the child runs
//! [`InheritStrip::apply`] (`close_range(3, ~0, CLOSE_RANGE_CLOEXEC)`, one
//! async-signal-safe syscall, with no ceiling at all) and then places stdio and
//! the lanes with `dup2`. Everything it calls between `fork` and `execve` is
//! async-signal-safe and every byte it reads was prepared before the fork.
//!
//! ## [`spawn`]: the same, with no lanes
//!
//! A long-lived helper this process starts and reads (`/usr/bin/log stream`)
//! would hold, through a plain `Command` on a std without SP-1, every
//! descriptor this process had open without close-on-exec at the spawn —
//! including, for its whole life, a bridge lane caught between `socketpair` and
//! its `F_SETFD`. [`spawn`] is the lane spawn with no lanes, and strips on any
//! std.
//!
//! ## [`InheritStrip`]: for the spawns that must still fork
//!
//! A PTY child needs `setrlimit` (the session sandbox) and a controlling
//! terminal (`TIOCSCTTY`, via `login_tty`), neither of which `posix_spawn` can
//! express, so it forks and strips; so does a `std` command that must run a
//! `pre_exec` hook anyway (the seamless successor's lanes,
//! [`InheritNothing`]). The strip LISTS the
//! child's own table (`PROC_PIDLISTFDS` on Darwin, `close_range` or
//! `/proc/self/fd` on Linux) and marks what it finds; where a listing fails or
//! is cut short it probes every number up to [`descriptor_bound`], the kernel's
//! per-process maximum — not the soft limit, which no longer bounds anything
//! here, and read before the fork, where no thread of an unprivileged process
//! can raise it. Preparing it also passes libnotify's once-gate
//! ([`pass_libnotify_gate`]), so no fork that strips can lose its child to
//! libSystem's own fork handler finding that gate half taken.
//!
//! ## [`command`]: every other child, `std::process::Command`'s shape, no fork
//!
//! The window starts short-lived helpers too — `osascript`, `open`, `sysctl`,
//! `lsof`, `codesign`, an atpkg pass behind `taskpolicy` — and on a std
//! without SP-1 (upstream's) a plain `Command` hands each of them every
//! descriptor this process holds without close-on-exec at that instant.
//! MEASURED 2026-10-01 (the pair race in this module's tests): a plain
//! `Command` racing another thread's `UnixStream::pair()` (Darwin has no
//! `SOCK_CLOEXEC`, so each end is unflagged until its `F_SETFD`) carried a pair
//! end into 17-27 of 200 children, and [`command`] into 0 of 400. (That test's
//! control is a raw leaky spawn, `test_support`: Trust's std hands a `Command`
//! child nothing above stdio since SP-1.) It is the one way product code
//! starts a child, and it strips on any std (`tools/grep_guard.sh` B27, whose
//! allow-list names the reviewed exceptions).
//!
//! [`command`] returns this module's [`Command`] — `std`'s shape and `std`'s
//! rules ([`no_fork`]'s docs list the few places it differs) — which on Darwin
//! is the lane spawn above: ONE `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`,
//! its stdio placed by file actions (a pipe's far end, a handed descriptor,
//! `/dev/null`, or this process's own slot by `addinherit_np`), its working
//! directory by `addchdir_np`, its process group, session and default signal
//! dispositions by spawn attributes, `exec` by `POSIX_SPAWN_SETEXEC`; the
//! `PATH` search is `posix_spawnp`'s, walked here so it reads the child's
//! `PATH`. Nothing runs between a fork and an exec, because there is no fork:
//! this process is never copied, so it holds nothing open in a copy (a `flock`
//! released only by closing its file stays released) and pays no bill after.
//! Off Darwin it starts through `std::process::Command` with [`InheritNothing`]
//! installed first — a fork, with `close_range` in the child on Linux.
//!
//! WHY NOT `std`'s `Command` WITH A HOOK: any `pre_exec`
//! hook takes `Command` off `posix_spawn` onto `fork`. MEASURED 2026-10-01
//! (`command_tests::bench_what_a_spawn_costs_a_window_sized_process`, a
//! process shaped like the window: 6 462 VM regions, 400 MB resident — the
//! window measured 6 585), `/usr/bin/true`, p50: the `spawn()` call took
//! 0.13 ms for `std` with no hook, 2.81 ms forked with the strip, and 0.15 ms
//! through [`command`]. The fork also leaves a bill AFTER `spawn` returns: it
//! write-protects every resident writable page of this process, so the next
//! write to each takes a fault on whichever thread writes it — rewriting those
//! 400 MB once took 22.9 ms after the forked spawn, against 0.40 ms after
//! `std`'s `posix_spawn` and 0.37 ms after [`command`] (0.26 ms with no spawn
//! at all). The review of 2026-10-01 measured the same 21–23 ms, and a
//! user-interactive thread writing 256 pages every 0.5 ms through a storm of
//! forked spawns going from p50 0.006 ms to 0.44 ms per pass: a dropped frame
//! per spawn.
//!
//! ## [`transparent`]: the pass-throughs, which hand on EVERYTHING
//!
//! A few verbs stand in for a program their caller ran and wait for it
//! (`aterm pkg lease -- <cmd>`, `aterm ship`): the caller's descriptors — a
//! make jobserver's pipe named in `MAKEFLAGS`, a `3>file` redirect, a number
//! handed down in the environment — belong to that program, so its child must
//! hold this process's WHOLE table. A `std::process::Command` no longer
//! promises that: since Trust's SP-1 (trust 6eb03c9a18) its child holds fds
//! 0-2 and only what `CommandExt::inherit_fd` names, and that and
//! `inherit_all_fds` are unstable (`trust_process_inherit_fds`), SP-1 std only.
//! [`transparent`] passes the table through whatever std this binary links: on
//! Darwin one `posix_spawn` WITHOUT `POSIX_SPAWN_CLOEXEC_DEFAULT`, elsewhere a
//! fork with no strip, the stdio slots untouched — upstream std's own spawn.
//! A pass-through that `exec`s needs none of this: an exec keeps the table on
//! every std (SP-1 leaves `CommandExt::exec` as it was).

#![cfg(unix)]

// The Linux arm's raw syscall numbers, its `struct rlimit` (two 64-bit
// `rlim_t`) and its signal constants are written for these three 64-bit
// architectures. Built for any other Linux, it would read a hard limit of 0
// and strip nothing — so it does not build there.
#[cfg(all(
    target_os = "linux",
    not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))
))]
compile_error!("spawnfd's Linux arm is written for x86_64, aarch64 and riscv64 only");

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

/// The two descriptor numbers the fabric bridge inherits: a verb connection and
/// a push (`subscribe`) connection. Shared by parent and child, since a bridge
/// listening on the wrong number looks exactly like one that never started.
pub const BRIDGE_VERB_FD: RawFd = 3;
pub const BRIDGE_PUSH_FD: RawFd = 4;

// The two numbers are the protocol; a mismatch yields a child that blocks on
// an empty descriptor and never speaks, so they are pinned at compile time.
const _: () = assert!(BRIDGE_VERB_FD == 3 && BRIDGE_PUSH_FD == 4);

/// The lowest number a source may sit on when the spawn places it: above every
/// target (stdio 0..=2 and the two lanes).
const FIRST_FREE: RawFd = BRIDGE_PUSH_FD + 1;

/// The first number a `posix_spawn` file action may NOT name: Darwin's libc
/// answers `EBADF` at add time for any descriptor at `OPEN_MAX` (10 240) or
/// above. (Linux forks here and has no such limit.)
#[cfg(target_vendor = "apple")]
const ACTION_FD_LIMIT: RawFd = 10_240;

/// Whether a source at `fd` can be placed as it is: above every target, and
/// (Darwin) at a number a file action may name.
fn placeable(fd: RawFd) -> bool {
    #[cfg(target_vendor = "apple")]
    {
        (FIRST_FREE..ACTION_FD_LIMIT).contains(&fd)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        fd >= FIRST_FREE
    }
}

/// One of a child's three stdio slots.
#[derive(Debug)]
pub enum ChildStdio {
    /// `/dev/null`: read-only as stdin, write-only as stdout or stderr.
    Null,
    /// This process's own descriptor at that number. A number this process does
    /// not have open is given `/dev/null` rather than left closed: a child born
    /// with stdout closed hands its first `open` that number.
    Inherit,
    /// This descriptor, placed at that number. Closed in this process once the
    /// spawn returns, like the lanes.
    Fd(OwnedFd),
}

/// The program [`spawn`] and [`spawn_with_two_fds`] start: its argv, its
/// environment and its stdio. The lanes are the spawn's own arguments.
///
/// It is started the way `execvp` starts a program. A bare program name (no
/// `/`) is searched on the `PATH` the CHILD will hold — the one in
/// [`LaneCommand::env_exactly`]'s set when that names one, else this
/// process's: the first candidate that starts wins (Darwin walks them with
/// `posix_spawn` as `posix_spawnp` does; Linux takes the first regular file
/// this process may execute, `access(X_OK)`, before its fork), one it may not
/// run is passed over (and reported, `EACCES`, only when nothing later
/// matches), and an unset `PATH` is `/usr/bin:/bin`. A file the kernel will
/// not exec as an image (`ENOEXEC`: a script with no `#!` line) is run by
/// `/bin/sh`, as `execvp` and `posix_spawnp` run it.
#[derive(Debug)]
pub struct LaneCommand {
    program: OsString,
    args: Vec<OsString>,
    env: Option<Vec<(OsString, OsString)>>,
    stdin: ChildStdio,
    stdout: ChildStdio,
    stderr: ChildStdio,
}

impl LaneCommand {
    /// `program` with no arguments, this process's environment, and stdio
    /// inherited — `Command::new`'s defaults.
    #[must_use]
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            env: None,
            stdin: ChildStdio::Inherit,
            stdout: ChildStdio::Inherit,
            stderr: ChildStdio::Inherit,
        }
    }

    /// Append one argument.
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    /// Append arguments.
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    /// The child's WHOLE environment — `env_clear` and `envs` in one call:
    /// nothing of this process's environment is added to it. Without this call
    /// the child gets this process's environment as it stands at the spawn.
    ///
    /// A key named twice is set once, to its LAST value, as `envs` does: a
    /// block carrying both would have the child's `getenv` read the first while
    /// the `PATH` search here read another.
    pub fn env_exactly<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let mut env: Vec<(OsString, OsString)> = Vec::new();
        for (k, v) in vars {
            let (k, v) = (k.as_ref(), v.as_ref().to_os_string());
            match env.iter_mut().find(|(key, _)| key.as_os_str() == k) {
                Some(slot) => slot.1 = v,
                None => env.push((k.to_os_string(), v)),
            }
        }
        self.env = Some(env);
        self
    }

    /// The child's stdin.
    pub fn stdin(&mut self, slot: ChildStdio) -> &mut Self {
        self.stdin = slot;
        self
    }

    /// The child's stdout.
    pub fn stdout(&mut self, slot: ChildStdio) -> &mut Self {
        self.stdout = slot;
        self
    }

    /// The child's stderr.
    pub fn stderr(&mut self, slot: ChildStdio) -> &mut Self {
        self.stderr = slot;
        self
    }
}

/// A child [`spawn_with_two_fds`] started: `std::process::Child`'s process
/// half, by pid.
///
/// It REAPS EXACTLY ONCE. The first `wait`/`try_wait` that collects the exit
/// keeps the status, and every later call answers from it without asking the
/// kernel again — a second `waitpid` could name a recycled pid. Until then the
/// pid is ours alone (an unreaped child keeps it, zombie or not), so
/// [`LaneChild::kill`] signals only our own child, and after the reap it
/// signals nothing. Dropping an unreaped handle collects a child that has
/// already exited (no zombie is left behind for it) and leaves a running one
/// running, as `std` does.
#[derive(Debug)]
pub struct LaneChild {
    pid: i32,
    status: Option<ExitStatus>,
}

impl LaneChild {
    /// The child's pid.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.pid.unsigned_abs()
    }

    /// Wait for the child to exit and reap it (once; see the type).
    ///
    /// # Errors
    /// `waitpid`'s, other than `EINTR` (retried).
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.reap(0)? {
                return Ok(status);
            }
        }
    }

    /// The exit status if the child has exited (reaping it), else `None`.
    ///
    /// # Errors
    /// `waitpid`'s, other than `EINTR` (retried).
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.reap(WNOHANG)
    }

    /// `SIGKILL` the child. A child already reaped is not signalled — its pid
    /// may name another process by now — and answers `Ok`, as `std` does.
    ///
    /// # Errors
    /// `kill`'s.
    pub fn kill(&mut self) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        // SAFETY: `kill` takes two integers; the pid is our own unreaped child,
        // which nothing else can have reused.
        if unsafe { kill(self.pid, SIGKILL) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// One `waitpid(pid, options)`, `EINTR` retried; the status once collected.
    fn reap(&mut self, options: i32) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        loop {
            let mut raw = 0;
            // SAFETY: `raw` is a live local `waitpid` writes; the pid is ours.
            let got = unsafe { waitpid(self.pid, &mut raw, options) };
            if got == self.pid {
                let status = ExitStatus::from_raw(raw);
                self.status = Some(status);
                return Ok(Some(status));
            }
            if got == 0 {
                return Ok(None);
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }
}

impl Drop for LaneChild {
    fn drop(&mut self) {
        let _ = self.try_wait();
    }
}

/// SPAWN `cmd` with `first` and `second` inherited at [`BRIDGE_VERB_FD`] and
/// [`BRIDGE_PUSH_FD`] — and nothing else above stdio.
///
/// Every descriptor handed in (the lanes, and a [`ChildStdio::Fd`]) is closed in
/// THIS process when the call returns, on the success and the failure path
/// alike — closing the parent's copies is what lets it see the child's EOF when
/// the bridge dies; a caller that kept one open would hold the far end forever
/// and the fail-closed halt would never fire.
///
/// # Errors
/// `InvalidInput` for a NUL in the program, an argument or the environment;
/// `NotFound` for a bare name no `PATH` entry holds (`PermissionDenied` when
/// the only ones it holds may not be executed); a descriptor that cannot be
/// moved off the target numbers; the spawn's own error (an `exec` failure
/// included — it is reported, never read as a started child).
pub fn spawn_with_two_fds(
    cmd: LaneCommand,
    first: OwnedFd,
    second: OwnedFd,
) -> io::Result<LaneChild> {
    let lanes = [off_the_targets(first)?, off_the_targets(second)?];
    launch(cmd, &lanes)
}

/// SPAWN `cmd` holding its stdio and NOTHING ELSE — no descriptor of this
/// process above stdio reaches it, whatever this process holds without
/// close-on-exec (Metal's shader-cache files; another thread's `socketpair` or
/// `pipe` caught between its creation and its `F_SETFD`). [`spawn_with_two_fds`]
/// with no lanes: the same exact inheritance, the same `exec` semantics, the
/// same handle — for a long-lived helper this process starts and reads, which
/// a plain `Command` on a std without SP-1 would hand every such descriptor for
/// its whole life.
///
/// # Errors
/// As [`spawn_with_two_fds`].
pub fn spawn(cmd: LaneCommand) -> io::Result<LaneChild> {
    launch(cmd, &[])
}

/// The spawn itself: `lanes` (already [`off_the_targets`]) at 3, 4, ….
fn launch(cmd: LaneCommand, lanes: &[OwnedFd]) -> io::Result<LaneChild> {
    let LaneCommand {
        program,
        args,
        env,
        stdin,
        stdout,
        stderr,
    } = cmd;
    let env = env.unwrap_or_else(|| std::env::vars_os().collect());
    let image = Image::build(&program, &args, &env)?;
    let stdio = [stage(stdin)?, stage(stdout)?, stage(stderr)?];
    let pid = platform::spawn(
        &image,
        &Placement {
            stdio: [stdio[0].source(), stdio[1].source(), stdio[2].source()],
            lanes,
            ..Placement::default()
        },
    )?;
    Ok(LaneChild { pid, status: None })
}

/// `slot` with its descriptor, if any, moved [`off_the_targets`].
fn stage(slot: ChildStdio) -> io::Result<ChildStdio> {
    match slot {
        ChildStdio::Fd(fd) => off_the_targets(fd).map(ChildStdio::Fd),
        other => Ok(other),
    }
}

impl ChildStdio {
    /// Where the spawn takes this slot from.
    fn source(&self) -> Source {
        match self {
            Self::Null => Source::Null,
            Self::Inherit => Source::Inherit,
            Self::Fd(fd) => Source::Fd(fd.as_raw_fd()),
        }
    }
}

/// Where one of the child's stdio slots comes from. Every descriptor is
/// [`placeable`] and stays open until the spawn returns.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// `/dev/null`: read-only as stdin, write-only as stdout or stderr.
    Null,
    /// This process's own descriptor at that number (`/dev/null` when it has
    /// none there).
    Inherit,
    /// This descriptor, placed at that number.
    Fd(RawFd),
}

/// How the child is placed, beyond its [`Image`]: stdio, the lanes at 3 and
/// up, and the attributes of the spawn. The lane spawns set only the first
/// two; [`Command`] sets the rest.
#[derive(Debug)]
struct Placement<'a> {
    stdio: [Source; 3],
    lanes: &'a [OwnedFd],
    /// The working directory the child starts in.
    cwd: Option<&'a std::ffi::CStr>,
    /// `cwd` made absolute: the directory a relative program or `PATH` entry
    /// is resolved against, here where it is checked as in the child.
    base: Option<&'a std::path::Path>,
    /// `setpgid(0, pgroup)` in the child.
    pgroup: Option<i32>,
    /// `setsid()` in the child.
    setsid: bool,
    /// Replace THIS process instead of starting a child (`exec`).
    replace: bool,
    /// Where `posix_spawnp` and `execvp` part: a `PATH` entry too long to join
    /// to a bare name ENDS the search with `ENAMETOOLONG` (`posix_spawnp`)
    /// instead of being passed over (`execvp`). `std` takes `posix_spawnp`
    /// exactly when the child's `PATH` is this process's own, so [`Command`]
    /// sets it then; the lane spawns keep `execvp`'s walk.
    long_entry_ends: bool,
    /// THE TRANSPARENT SPAWN ([`transparent`]): the child inherits this
    /// process's whole table as it stands — no `POSIX_SPAWN_CLOEXEC_DEFAULT`
    /// on Darwin, no strip off it — an inherited stdio slot is left exactly
    /// as this process holds it, and the spawning thread's signal mask is
    /// kept: `std`'s own spawn before SP-1, where every other spawn here
    /// differs on purpose. `SIGPIPE` still goes back to its default, as
    /// `std` resets it.
    inherit_all: bool,
}

impl Default for Placement<'_> {
    fn default() -> Self {
        Self {
            stdio: [Source::Inherit; 3],
            lanes: &[],
            cwd: None,
            base: None,
            pgroup: None,
            setsid: false,
            replace: false,
            long_entry_ends: false,
            inherit_all: false,
        }
    }
}

/// `fd`, or — when it sits on a target number (0..=4), or at a number a file
/// action may not name — a fresh `F_DUPFD_CLOEXEC` copy at [`FIRST_FREE`] or
/// above, the original closed.
///
/// One rule instead of a case per collision: a source on its own target would
/// be a `dup2(fd, fd)`, which keeps close-on-exec (the lane arrives closed); a
/// source on ANOTHER target is clobbered by that target's placement before its
/// own is read; and Darwin's libc refuses a file action at 10 240 or above.
/// Above every target and below the limit, none of the three can happen.
///
/// # Errors
/// `fcntl`'s, or `EMFILE` when no number below the limit is free.
fn off_the_targets(fd: OwnedFd) -> io::Result<OwnedFd> {
    if placeable(fd.as_raw_fd()) {
        return Ok(fd);
    }
    placeable_copy(fd.as_raw_fd())
}

/// `fd` — a descriptor THIS process keeps across the spawn (its end of a
/// child's pipe) — or, when it sits on a stdio number (0..=2), a fresh copy
/// off them ([`placeable_copy`]), the original closed.
///
/// `io::pipe()` takes the lowest free numbers, so in a process that has closed
/// its stdin, stdout or stderr our own end lands on that number. Left there it
/// reads, to a later `Inherit` slot, as this process's own stdio: the spawn
/// hands it to the child (`addinherit_np`), and a child given the write end of
/// its own stdin never sees that stdin end (review of 2026-10-01). Off 0..=2,
/// the closed slot reads as closed and the child gets `/dev/null` there.
#[cfg(target_vendor = "apple")]
fn off_stdio(fd: OwnedFd) -> io::Result<OwnedFd> {
    if fd.as_raw_fd() > 2 {
        return Ok(fd);
    }
    placeable_copy(fd.as_raw_fd())
}

/// A fresh `F_DUPFD_CLOEXEC` copy of `fd` at [`FIRST_FREE`] or above, at a
/// number a file action may name; `fd` itself is left as it is.
///
/// # Errors
/// `fcntl`'s, or `EMFILE` when no number below the limit is free.
fn placeable_copy(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: `F_DUPFD_CLOEXEC` reads one descriptor number and returns a
    // fresh descriptor or -1; it touches no memory.
    let copy = unsafe { fcntl(fd, F_DUPFD_CLOEXEC, FIRST_FREE) };
    if copy < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `copy` was just created here and nothing else owns it.
    let copy = unsafe { OwnedFd::from_raw_fd(copy) };
    if !placeable(copy.as_raw_fd()) {
        return Err(io::Error::from_raw_os_error(EMFILE));
    }
    Ok(copy)
}

/// Everything the exec reads, built (and allocated) before the spawn: the
/// program as it was named, argv, the environment block, and the `PATH` that
/// block holds — the one a bare name is searched on, as `execvp` searches
/// the environment it is about to install.
struct Image {
    program: CString,
    argv: Vec<CString>,
    envp: Vec<CString>,
    search: Option<OsString>,
}

/// The shell an `ENOEXEC` image is handed to (`_PATH_BSHELL`).
const BSHELL: &core::ffi::CStr = c"/bin/sh";
/// `ENOEXEC`: an image the kernel will not exec (a script with no `#!`).
const ENOEXEC: i32 = 8;

impl Image {
    /// `program` with `args` and exactly the environment `env` (in order; the
    /// first `PATH` in it is the one `getenv` will read, so the one searched).
    ///
    /// # Errors
    /// `InvalidInput` for a NUL anywhere in them — `std`'s error and message.
    fn build(program: &OsStr, args: &[OsString], env: &[(OsString, OsString)]) -> io::Result<Self> {
        let search = env
            .iter()
            .find(|(k, _)| k.as_bytes() == b"PATH")
            .map(|(_, v)| v.clone());
        let program = c_string(program.as_bytes())?;
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(program.clone());
        for arg in args {
            argv.push(c_string(arg.as_bytes())?);
        }
        let mut envp = Vec::with_capacity(env.len());
        for (k, v) in env {
            let mut kv = Vec::with_capacity(k.len() + v.len() + 1);
            kv.extend_from_slice(k.as_bytes());
            kv.push(b'=');
            kv.extend_from_slice(v.as_bytes());
            envp.push(c_string(&kv)?);
        }
        Ok(Self {
            program,
            argv,
            envp,
            search,
        })
    }

    /// The argv `/bin/sh` is handed for an image the kernel answered
    /// `ENOEXEC` for: `sh <path> <args…>`, as `execvp` and `posix_spawnp`
    /// build it.
    fn script(&self, path: &CString) -> Vec<CString> {
        let mut script = Vec::with_capacity(self.argv.len() + 1);
        script.push(c"sh".to_owned());
        script.push(path.clone());
        script.extend(self.argv.iter().skip(1).cloned());
        script
    }

    /// `strings` as the NULL-terminated pointer array `exec` reads; the
    /// pointers live as long as `strings`.
    fn pointers(strings: &[CString]) -> Vec<*const core::ffi::c_char> {
        strings
            .iter()
            .map(|s| s.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect()
    }
}

fn c_string(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "nul byte found in provided data",
        )
    })
}

/// The Linux lane spawn's search, made before its fork: the file `program`
/// names — itself when it holds a `/`, else the first regular file in a
/// `path_var` directory that this process may execute (an empty entry is the
/// current directory; no `PATH` is `/usr/bin:/bin`, `execvp`'s default). A
/// file it may not execute is passed over, as `execvp` passes over a
/// candidate its `execve` answered `EACCES`, and is the error only when
/// nothing later matches. (Darwin needs no answer before its spawn: it walks
/// the candidates with `posix_spawn` itself, as `posix_spawnp` does.)
#[cfg(not(target_vendor = "apple"))]
fn resolve(program: &OsStr, path_var: Option<&OsStr>) -> io::Result<CString> {
    if program.as_bytes().contains(&b'/') {
        return c_string(program.as_bytes());
    }
    if program.is_empty() {
        return Err(io::Error::from_raw_os_error(ENOENT));
    }
    let search = path_var.unwrap_or_else(|| OsStr::new("/usr/bin:/bin"));
    let mut denied = false;
    for dir in std::env::split_paths(search) {
        let dir = if dir.as_os_str().is_empty() {
            std::path::PathBuf::from(".")
        } else {
            dir
        };
        let candidate = dir.join(program);
        if !std::fs::metadata(&candidate).is_ok_and(|md| md.is_file()) {
            continue;
        }
        let candidate = c_string(candidate.as_os_str().as_bytes())?;
        // SAFETY: a NUL-terminated path; `access` reads it and retains nothing.
        if unsafe { access(candidate.as_ptr(), X_OK) } == 0 {
            return Ok(candidate);
        }
        denied = true;
    }
    Err(io::Error::from_raw_os_error(if denied {
        EACCES
    } else {
        ENOENT
    }))
}

#[cfg(target_vendor = "apple")]
mod platform {
    use super::{BSHELL, ENOEXEC, F_GETFD, Image, Placement, Source, c_string, fcntl};
    use core::ffi::{CStr, c_char, c_void};
    use std::ffi::{CString, OsStr};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    type Attr = *mut c_void;
    type Actions = *mut c_void;

    // <sys/spawn.h>, read from the SDK 2026-09-30 (SETPGROUP, SETEXEC and
    // SETSID 2026-10-01).
    const POSIX_SPAWN_SETPGROUP: i16 = 0x0002;
    const POSIX_SPAWN_SETSIGDEF: i16 = 0x0004;
    const POSIX_SPAWN_SETSIGMASK: i16 = 0x0008;
    const POSIX_SPAWN_SETEXEC: i16 = 0x0040;
    const POSIX_SPAWN_SETSID: i16 = 0x0400;
    const POSIX_SPAWN_CLOEXEC_DEFAULT: i16 = 0x4000;
    const O_RDONLY: i32 = 0;
    const O_WRONLY: i32 = 1;
    /// `sigset_t` is a 32-bit mask on Darwin, signal `n` at bit `n - 1`.
    const SIGPIPE_BIT: u32 = 1 << (13 - 1);
    /// `<sys/syslimits.h>`: the buffer `posix_spawnp` builds a candidate in.
    const PATH_MAX: usize = 1024;
    // <sys/errno.h>: the answers `posix_spawnp` sorts.
    const ENOENT: i32 = 2;
    const E2BIG: i32 = 7;
    const ENOMEM: i32 = 12;
    const EACCES: i32 = 13;
    const ENOTDIR: i32 = 20;
    const ETXTBSY: i32 = 26;
    const ELOOP: i32 = 62;
    const ENAMETOOLONG: i32 = 63;

    unsafe extern "C" {
        fn posix_spawnattr_init(attr: *mut Attr) -> i32;
        fn posix_spawnattr_destroy(attr: *mut Attr) -> i32;
        fn posix_spawnattr_setflags(attr: *mut Attr, flags: i16) -> i32;
        fn posix_spawnattr_setsigmask(attr: *mut Attr, mask: *const u32) -> i32;
        fn posix_spawnattr_setsigdefault(attr: *mut Attr, mask: *const u32) -> i32;
        fn posix_spawnattr_setpgroup(attr: *mut Attr, pgroup: i32) -> i32;
        fn posix_spawn_file_actions_init(actions: *mut Actions) -> i32;
        fn posix_spawn_file_actions_destroy(actions: *mut Actions) -> i32;
        fn posix_spawn_file_actions_addopen(
            actions: *mut Actions,
            fd: i32,
            path: *const c_char,
            oflag: i32,
            mode: u16,
        ) -> i32;
        fn posix_spawn_file_actions_adddup2(actions: *mut Actions, fd: i32, new: i32) -> i32;
        fn posix_spawn_file_actions_addinherit_np(actions: *mut Actions, fd: i32) -> i32;
        fn posix_spawn_file_actions_addchdir_np(actions: *mut Actions, path: *const c_char) -> i32;
        fn posix_spawn(
            pid: *mut i32,
            path: *const c_char,
            actions: *const Actions,
            attr: *const Attr,
            argv: *const *const c_char,
            envp: *const *const c_char,
        ) -> i32;
    }

    /// The attribute and file-action objects, destroyed on every path.
    struct Objects {
        attr: Attr,
        actions: Actions,
    }

    impl Objects {
        fn new() -> io::Result<Self> {
            let mut attr: Attr = std::ptr::null_mut();
            // SAFETY: init writes a fresh attribute object into `attr`.
            check(unsafe { posix_spawnattr_init(&mut attr) })?;
            let mut actions: Actions = std::ptr::null_mut();
            // SAFETY: init writes a fresh file-actions object into `actions`.
            if let Err(e) = check(unsafe { posix_spawn_file_actions_init(&mut actions) }) {
                // SAFETY: `attr` was initialised above and is destroyed once.
                unsafe { posix_spawnattr_destroy(&mut attr) };
                return Err(e);
            }
            Ok(Self { attr, actions })
        }

        /// One `posix_spawn` of `path` with these objects: the child's pid,
        /// or the errno it answered.
        fn spawn(
            &self,
            path: &CStr,
            argv: &[*const c_char],
            envp: &[*const c_char],
        ) -> Result<i32, i32> {
            let mut pid = 0;
            // SAFETY: both objects were initialised by `new`; `path` is a C
            // string and `argv`/`envp` NULL-terminated arrays of C strings,
            // all of which outlive the call and none of which it retains.
            let rc = unsafe {
                posix_spawn(
                    &mut pid,
                    path.as_ptr(),
                    &self.actions,
                    &self.attr,
                    argv.as_ptr(),
                    envp.as_ptr(),
                )
            };
            if rc == 0 { Ok(pid) } else { Err(rc) }
        }
    }

    impl Drop for Objects {
        fn drop(&mut self) {
            // SAFETY: both were initialised by `new` and are destroyed once.
            unsafe {
                posix_spawn_file_actions_destroy(&mut self.actions);
                posix_spawnattr_destroy(&mut self.attr);
            }
        }
    }

    fn check(rc: i32) -> io::Result<()> {
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(rc))
        }
    }

    /// `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`: the child holds stdio
    /// and the lanes and nothing else. Every source is already at 5..10 240
    /// (`off_the_targets`), so the actions below never self-copy, never clobber
    /// a source a later action reads, and never name a number libc refuses.
    /// The signal mask is emptied and `SIGPIPE` is reset to its default — the
    /// disposition `Command` resets; `std` now leaves the mask as the spawning
    /// thread's, which in the window blocks `SIGUSR1`, so a child here never
    /// starts with a signal blocked. The working directory, the process group
    /// and the session are the spawn's own attributes; `replace` turns the
    /// spawn into an `exec` of THIS process (`POSIX_SPAWN_SETEXEC`), which
    /// returns only on failure.
    ///
    /// `inherit_all` ([`super::transparent`]) is the one spawn without
    /// `POSIX_SPAWN_CLOEXEC_DEFAULT`: the kernel then hands the child every
    /// descriptor this process holds without close-on-exec, an inherited
    /// stdio slot is named by no file action (it reaches the child exactly as
    /// it is here, as `std`'s `Command` hands it), and the spawning thread's
    /// signal mask is kept (`std` keeps it). `SIGPIPE` still goes back to its
    /// default, as `std` resets it.
    pub(super) fn spawn(image: &Image, place: &Placement<'_>) -> io::Result<i32> {
        let argv = Image::pointers(&image.argv);
        let envp = Image::pointers(&image.envp);
        let mut objects = Objects::new()?;
        let empty: u32 = 0;
        let pipe_only: u32 = SIGPIPE_BIT;
        let mut flags = POSIX_SPAWN_SETSIGDEF;
        if !place.inherit_all {
            flags |= POSIX_SPAWN_CLOEXEC_DEFAULT | POSIX_SPAWN_SETSIGMASK;
        }
        if place.pgroup.is_some() {
            flags |= POSIX_SPAWN_SETPGROUP;
        }
        if place.setsid {
            flags |= POSIX_SPAWN_SETSID;
        }
        if place.replace {
            flags |= POSIX_SPAWN_SETEXEC;
        }
        // SAFETY: each call takes an object `Objects::new` initialised and a
        // live local or C string; none retains a pointer past the call.
        unsafe {
            check(posix_spawnattr_setflags(&mut objects.attr, flags))?;
            if !place.inherit_all {
                check(posix_spawnattr_setsigmask(&mut objects.attr, &empty))?;
            }
            check(posix_spawnattr_setsigdefault(&mut objects.attr, &pipe_only))?;
            if let Some(pgroup) = place.pgroup {
                check(posix_spawnattr_setpgroup(&mut objects.attr, pgroup))?;
            }
            for (target, slot) in (0..).zip(place.stdio) {
                let null_mode = if target == 0 { O_RDONLY } else { O_WRONLY };
                let open_null = |actions: *mut Actions| {
                    posix_spawn_file_actions_addopen(
                        actions,
                        target,
                        c"/dev/null".as_ptr(),
                        null_mode,
                        0,
                    )
                };
                check(match slot {
                    Source::Null => open_null(&mut objects.actions),
                    // Transparent: the slot as this process holds it, by no
                    // file action at all (`std`'s spawn names none for it).
                    Source::Inherit if place.inherit_all => 0,
                    // `addinherit_np` clears the flag: a close-on-exec stdio
                    // of ours still reaches a spawned child, as `Inherit` says
                    // (not under `SETEXEC`: an exec is handed a copy without
                    // the flag, `Command::launch`).
                    Source::Inherit if fcntl(target, F_GETFD) >= 0 => {
                        posix_spawn_file_actions_addinherit_np(&mut objects.actions, target)
                    }
                    Source::Inherit => open_null(&mut objects.actions),
                    Source::Fd(fd) => {
                        posix_spawn_file_actions_adddup2(&mut objects.actions, fd, target)
                    }
                })?;
            }
            for (target, lane) in (super::BRIDGE_VERB_FD..).zip(place.lanes) {
                check(posix_spawn_file_actions_adddup2(
                    &mut objects.actions,
                    lane.as_raw_fd(),
                    target,
                ))?;
            }
            if let Some(cwd) = place.cwd {
                check(posix_spawn_file_actions_addchdir_np(
                    &mut objects.actions,
                    cwd.as_ptr(),
                ))?;
            }
        }
        spawnp(image, place, &objects, &argv, &envp)
    }

    /// `posix_spawnp(3)` and `execvp(3)`, as this Mac answers them (MEASURED
    /// 2026-10-01 with both, the same answers): a program holding a `/` is
    /// tried once, and its error is the answer as the kernel gave it
    /// (`ENOTDIR` for a file on the way, `EACCES` for an unsearchable
    /// directory). A bare name is tried in every entry of the `PATH` the CHILD
    /// will hold (an empty entry is the current directory; no `PATH` is
    /// `/usr/bin:/bin`): `ENOENT`, `ENOTDIR`, `ELOOP` and `ENAMETOOLONG` move
    /// on, a candidate that exists but is refused `EACCES` (a file it may not
    /// run, a directory) moves on and is the answer only if nothing later
    /// starts, and any other answer is final; nothing found is `ENOENT`. Either
    /// way `ENOEXEC` hands the file to `/bin/sh`. The two part at an entry too
    /// long to join to the name (`PATH_MAX`): `execvp` passes it over,
    /// `posix_spawnp` ends there with `ENAMETOOLONG` — the walk
    /// `Placement::long_entry_ends` picks, as `std` would.
    ///
    /// Two deliberate differences, neither visible to a caller: a candidate
    /// that does not exist is passed over HERE (a `stat`, the check
    /// `posix_spawnp` itself makes after a failure) instead of costing a
    /// spawn attempt; and with a working directory set, a relative candidate
    /// is joined to it first ([`rebase`]) — so it names the file the child
    /// would, and Darwin's `addchdir_np` never sees a relative image (with
    /// one, `posix_spawn` starts the program and still answers `ENOENT`:
    /// `std`'s note, which sends `Command` to `fork` for that case). A `#!`
    /// script reached that way sees the joined path as `$0`.
    fn spawnp(
        image: &Image,
        place: &Placement<'_>,
        objects: &Objects,
        argv: &[*const c_char],
        envp: &[*const c_char],
    ) -> io::Result<i32> {
        let as_script = |candidate: &CString| {
            let script = image.script(candidate);
            let script = Image::pointers(&script);
            objects
                .spawn(BSHELL, &script, envp)
                .map_err(io::Error::from_raw_os_error)
        };
        let program = image.program.as_bytes();
        if program.contains(&b'/') {
            let path = rebase(program, place)?;
            return match objects.spawn(&path, argv, envp) {
                Ok(pid) => Ok(pid),
                Err(ENOEXEC) => as_script(&path),
                Err(rc) => Err(io::Error::from_raw_os_error(rc)),
            };
        }
        if program.is_empty() {
            return Err(io::Error::from_raw_os_error(ENOENT));
        }
        let search = image
            .search
            .as_deref()
            .map_or(&b"/usr/bin:/bin"[..], OsStr::as_bytes);
        let exists = |path: &CString| std::fs::metadata(OsStr::from_bytes(path.as_bytes())).is_ok();
        let mut eacces = false;
        for dir in search.split(|&b| b == b':') {
            let dir: &[u8] = if dir.is_empty() { b"." } else { dir };
            // Too long for the libc's `PATH_MAX` buffer: `execvp` passes the
            // entry over, `posix_spawnp` stops there with `ENAMETOOLONG`
            // (MEASURED 2026-10-01 on `/usr/bin:/bin`: from a 1 015-byte
            // name on, `posix_spawnp` answers 63 and `execvp` 2).
            if dir.len() + program.len() + 2 > PATH_MAX {
                if place.long_entry_ends {
                    return Err(io::Error::from_raw_os_error(ENAMETOOLONG));
                }
                continue;
            }
            let mut full = Vec::with_capacity(dir.len() + program.len() + 1);
            full.extend_from_slice(dir);
            full.push(b'/');
            full.extend_from_slice(program);
            let candidate = rebase(&full, place)?;
            if !exists(&candidate) {
                continue;
            }
            let rc = match objects.spawn(&candidate, argv, envp) {
                Ok(pid) => return Ok(pid),
                Err(rc) => rc,
            };
            match rc {
                ELOOP | ENAMETOOLONG | ENOENT | ENOTDIR => {}
                ENOEXEC => return as_script(&candidate),
                E2BIG | ENOMEM | ETXTBSY => return Err(io::Error::from_raw_os_error(rc)),
                _ if !exists(&candidate) => {}
                EACCES => eacces = true,
                _ => return Err(io::Error::from_raw_os_error(rc)),
            }
        }
        Err(io::Error::from_raw_os_error(if eacces {
            EACCES
        } else {
            ENOENT
        }))
    }

    /// `path` as the child resolves it: joined to the spawn's working
    /// directory (absolute) when it is relative and the spawn sets one, else
    /// as it is — relative to this process's directory, which the child
    /// shares.
    fn rebase(path: &[u8], place: &Placement<'_>) -> io::Result<CString> {
        match place.base {
            Some(base) if path.first() != Some(&b'/') => {
                let base = base.as_os_str().as_bytes();
                let mut full = Vec::with_capacity(base.len() + path.len() + 1);
                full.extend_from_slice(base);
                full.push(b'/');
                full.extend_from_slice(path);
                c_string(&full)
            }
            _ => c_string(path),
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{
        F_GETFD, F_SETFD, Image, InheritStrip, Placement, Source, fcntl, off_the_targets, resolve,
        waitpid,
    };
    use core::ffi::{c_char, c_void};
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;

    const O_RDWR: i32 = 2;
    const O_CLOEXEC: i32 = 0o2_000_000;
    const SIG_SETMASK: i32 = 2;
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    /// The target is this process's own stdio slot.
    const INHERIT: RawFd = -1;

    unsafe extern "C" {
        fn fork() -> i32;
        fn execve(
            path: *const c_char,
            argv: *const *const c_char,
            envp: *const *const c_char,
        ) -> i32;
        fn pipe2(fds: *mut RawFd, flags: i32) -> i32;
        fn open(path: *const c_char, flags: i32, ...) -> RawFd;
        fn read(fd: RawFd, buf: *mut c_void, count: usize) -> isize;
        fn write(fd: RawFd, buf: *const c_void, count: usize) -> isize;
        fn dup2(old: RawFd, new: RawFd) -> RawFd;
        fn sigprocmask(how: i32, set: *const u64, old: *mut u64) -> i32;
        fn signal(sig: i32, handler: usize) -> usize;
        fn _exit(code: i32) -> !;
    }

    /// Fork, strip, place, exec — the child calling only async-signal-safe
    /// functions on memory prepared here. Its exec status comes back on a
    /// close-on-exec pipe of OUR making, whose write end sits at 5 or above
    /// like every source, so no placement can clobber it: EOF is a started
    /// child, four bytes are the errno it died of before `execve` took.
    ///
    /// It places stdio and lanes only: `Command`, the one spawn that sets a
    /// working directory, a process group, a session, an `exec` or
    /// `posix_spawnp`'s walk, starts through `std` here (`spawnfd_command.rs`).
    ///
    /// `inherit_all` ([`super::transparent`]) forks with NO strip: the child
    /// keeps every descriptor this process holds without close-on-exec, an
    /// inherited stdio slot is left as it is, and the signal mask is the
    /// forking thread's — `std`'s own fork, less `std`.
    pub(super) fn spawn(image: &Image, place: &Placement<'_>) -> io::Result<i32> {
        if place.cwd.is_some()
            || place.base.is_some()
            || place.pgroup.is_some()
            || place.setsid
            || place.replace
            || place.long_entry_ends
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the Linux lane spawn places stdio and lanes only",
            ));
        }
        let (stdio, lanes) = (&place.stdio, place.lanes);
        let path = resolve(
            std::ffi::OsStr::from_bytes(image.program.as_bytes()),
            image.search.as_deref(),
        )?;
        let script = image.script(&path);
        let argv = Image::pointers(&image.argv);
        let envp = Image::pointers(&image.envp);
        let script = Image::pointers(&script);
        let strip = (!place.inherit_all).then(InheritStrip::prepare);
        // SAFETY: a C string literal; the result is a fresh descriptor or -1.
        let null = unsafe { open(c"/dev/null".as_ptr(), O_RDWR | O_CLOEXEC) };
        if null < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: just opened here, owned by nothing else.
        let null = off_the_targets(unsafe { OwnedFd::from_raw_fd(null) })?;
        let mut ends: [RawFd; 2] = [-1, -1];
        // SAFETY: `pipe2` fills the two-element array it is handed.
        if unsafe { pipe2(ends.as_mut_ptr(), O_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both ends were just created here and nothing else owns them.
        let (status_rd, status_wr) =
            unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
        let status_wr = off_the_targets(status_wr)?;
        // Target `n` takes `sources[n]`: stdio, then the lanes at 3 and 4.
        let stdio_fds = stdio.iter().map(|slot| match *slot {
            Source::Null => null.as_raw_fd(),
            Source::Inherit => INHERIT,
            Source::Fd(fd) => fd,
        });
        let mut sources = [INHERIT; 5];
        let mut placed = 0;
        for (source, from) in sources
            .iter_mut()
            .zip(stdio_fds.chain(lanes.iter().map(AsRawFd::as_raw_fd)))
        {
            *source = from;
            placed += 1;
        }
        let (null_fd, report) = (null.as_raw_fd(), status_wr.as_raw_fd());
        let exec = Exec {
            path: &path,
            argv: &argv,
            envp: &envp,
            script: &script,
        };
        // SAFETY: `fork` takes no arguments. The child branch never returns.
        let pid = unsafe { fork() };
        if pid == 0 {
            // SAFETY: the forked child; see `child`.
            unsafe { child(strip, &sources, placed, null_fd, report, &exec) }
        }
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        drop(status_wr);
        let mut word = [0u8; 4];
        let mut got = 0;
        while got < word.len() {
            let rest = &mut word[got..];
            // SAFETY: `rest` is a live, writable slice of `word`.
            let n = unsafe { read(status_rd.as_raw_fd(), rest.as_mut_ptr().cast(), rest.len()) };
            if n == 0 {
                break;
            }
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            got += n.unsigned_abs();
        }
        if got == 0 {
            return Ok(pid);
        }
        // It died before `execve` took: reap it here, nobody else will.
        let mut raw = 0;
        // SAFETY: our own child; `raw` is a live local.
        while unsafe { waitpid(pid, &mut raw, 0) } < 0
            && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
        {}
        Err(if got == word.len() {
            io::Error::from_raw_os_error(i32::from_be_bytes(word))
        } else {
            io::Error::other("the child's exec-status report was cut short")
        })
    }

    /// What the child execs, every pointer built in the parent.
    struct Exec<'a> {
        path: &'a std::ffi::CStr,
        argv: &'a [*const c_char],
        envp: &'a [*const c_char],
        script: &'a [*const c_char],
    }

    /// THE FORKED CHILD. Async-signal-safe calls only (`close_range`/`fcntl`,
    /// `dup2`, `sigprocmask`, `signal`, `execve`, `write`, `_exit`), on memory
    /// the parent prepared; it allocates nothing and never returns. An image
    /// the kernel answers `ENOEXEC` for is handed to `/bin/sh`, as `execvp`
    /// hands it. With no `strip` (the transparent spawn) it marks nothing,
    /// leaves an inherited stdio slot and the signal mask as they are, and
    /// still resets `SIGPIPE`.
    ///
    /// # Safety
    /// Only between `fork` and `exec`, with `sources[..placed]` at 5 or above
    /// (or `INHERIT`), `null` and `report` open, and `exec`'s pointers live.
    unsafe fn child(
        strip: Option<InheritStrip>,
        sources: &[RawFd; 5],
        placed: usize,
        null: RawFd,
        report: RawFd,
        exec: &Exec<'_>,
    ) -> ! {
        // SAFETY (whole body): the function's contract.
        unsafe {
            let transparent = strip.is_none();
            if let Some(strip) = strip {
                strip.apply();
            }
            for (target, &source) in (0..).zip(sources.iter().take(placed)) {
                let placed = if source != INHERIT {
                    dup2(source, target)
                } else if transparent {
                    0
                } else if fcntl(target, F_GETFD) < 0 {
                    dup2(null, target)
                } else {
                    fcntl(target, F_SETFD, 0)
                };
                if placed < 0 {
                    fail(report);
                }
            }
            if !transparent {
                let empty = [0u64; 16];
                sigprocmask(SIG_SETMASK, empty.as_ptr(), std::ptr::null_mut());
            }
            signal(SIGPIPE, SIG_DFL);
            execve(exec.path.as_ptr(), exec.argv.as_ptr(), exec.envp.as_ptr());
            if io::Error::last_os_error().raw_os_error() == Some(super::ENOEXEC) {
                execve(
                    super::BSHELL.as_ptr(),
                    exec.script.as_ptr(),
                    exec.envp.as_ptr(),
                );
            }
            fail(report)
        }
    }

    /// Report this child's errno on the status pipe and exit.
    ///
    /// # Safety
    /// Only in the forked child.
    unsafe fn fail(report: RawFd) -> ! {
        let errno = io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let word = errno.to_be_bytes();
        // SAFETY: `write` and `_exit` are async-signal-safe; `word` is live.
        unsafe {
            write(report, word.as_ptr().cast(), word.len());
            _exit(127)
        }
    }
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
mod platform {
    use super::{Image, Placement};
    use std::io;

    pub(super) fn spawn(_image: &Image, _place: &Placement<'_>) -> io::Result<i32> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "spawnfd's spawn is built for Darwin and Linux",
        ))
    }
}

/// THE STRIP, for a spawn that must fork (a PTY child: `setrlimit` and a
/// controlling terminal are not `posix_spawn` file actions). [`Self::prepare`]
/// in the parent, before the fork; [`Self::apply`] in the child, between the
/// fork and the exec: every descriptor above stdio is then close-on-exec, and
/// what the child places with `dup2` afterwards is not.
///
/// Marking rather than closing is deliberate: a status pipe the child reports
/// a failure on is already close-on-exec and must stay open until the exec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InheritStrip {
    bound: RawFd,
}

impl InheritStrip {
    /// Read [`descriptor_bound`] — in the parent, before the fork — and pass
    /// libnotify's once-gate ([`pass_libnotify_gate`]), so no fork that strips
    /// (aterm-pty's spawn, [`InheritNothing`], the update hand-off's fork lane)
    /// can catch that gate half taken and lose its child inside `fork()`.
    #[must_use]
    pub fn prepare() -> Self {
        pass_libnotify_gate();
        Self {
            bound: descriptor_bound(),
        }
    }

    /// How far a marking loop reaches (exclusive).
    #[must_use]
    pub fn bound(self) -> RawFd {
        self.bound
    }

    /// MARK EVERY DESCRIPTOR FROM 3 UP CLOSE-ON-EXEC. Call it in a forked
    /// child before its `exec` (or right before an `exec` that replaces this
    /// process); anywhere else it would strip descriptors the process means to
    /// pass on.
    ///
    /// The descriptors are LISTED, not probed, wherever the kernel can list
    /// them — the child's own table, which nothing else can change while it
    /// runs:
    ///
    /// * Linux: `close_range(3, ~0, CLOSE_RANGE_CLOEXEC)` (5.11+), else the
    ///   numbers `/proc/self/fd` lists (read with `getdents64` into a stack
    ///   buffer).
    /// * Darwin: the numbers `proc_pidinfo(PROC_PIDLISTFDS)` lists, into a
    ///   stack buffer of [`darwin_strip::SLOTS`] entries — ascending, so a
    ///   full buffer vouches for every number up to its last entry and the
    ///   loop below covers only the rest, up to the END OF THE TABLE (the
    ///   size a listing with no buffer answers: no open number reaches past
    ///   it, and a forked child's table ends just past its highest number).
    ///   MEASURED 2026-10-01, fork + strip + exec of `/usr/bin/true` on this
    ///   Mac: 1.18–1.26 ms against 1.18 ms with no strip at all; the loop to
    ///   `kern.maxfilesperproc` (92 160 `fcntl`s) cost 9.3 ms against
    ///   1.25 ms — ~8 ms on every shell spawn — and looping over the rest of
    ///   a full listing to the bound cost a spawn with 2 100 descriptors open
    ///   10–13 ms against 2.3 ms with 2 000, and 0.9–1.0 ms for either once
    ///   the loop stopped at the table's end.
    ///
    /// The last resort, where the listing fails or is cut short: one
    /// `fcntl(F_SETFD)` per number left (below the bound, or the table's
    /// end); an unused number is a harmless `EBADF`.
    ///
    /// ASYNC-SIGNAL-SAFE by construction: raw syscalls and integer arithmetic,
    /// no allocation, no lock, no `static`, no error formatting.
    pub fn apply(self) {
        #[cfg(target_os = "linux")]
        if linux_strip::close_range_cloexec() || linux_strip::proc_self_fd() {
            return;
        }
        #[cfg(target_vendor = "apple")]
        let rest = darwin_strip::mark_listed(self.bound);
        #[cfg(not(target_vendor = "apple"))]
        let rest = 3..self.bound;
        for fd in rest {
            // SAFETY: `F_SETFD` takes an int and touches no memory; an unused
            // number is a harmless `EBADF`.
            unsafe {
                fcntl(fd, F_SETFD, FD_CLOEXEC);
            }
        }
    }
}

/// A `std::process::Command` whose child inherits NOTHING of this process above
/// stdio: [`InheritStrip`] in a `pre_exec` hook — for a spawn that must fork
/// anyway (the module docs say what that costs and why stdio survives it).
/// Product code that needs no code between the fork and the exec uses
/// [`command`], which does not fork at all on Darwin.
pub trait InheritNothing {
    /// Install the strip. Hooks run in the order they were added, so install
    /// it before any hook that hands a descriptor over, or that hook's work is
    /// undone.
    ///
    /// With `exec` (which replaces THIS process) the strip marks this
    /// process's own table, as [`InheritStrip::apply`] allows right before
    /// such an `exec` — and a FAILED exec does not undo it: the process that
    /// survives holds every descriptor close-on-exec, so nothing it later
    /// spawns or execs inherits one unless it clears that flag again. Use it
    /// with `exec` only where the error path hands nothing on (the shipped
    /// `exec` sites are B27's allow-listed raw ones, none through here).
    fn inherit_nothing(&mut self) -> &mut Self;
}

impl InheritNothing for std::process::Command {
    fn inherit_nothing(&mut self) -> &mut Self {
        use std::os::unix::process::CommandExt as _;
        // The bound is read here, before any fork, where no thread can raise it.
        let strip = InheritStrip::prepare();
        // SAFETY: the hook runs between fork and exec (or right before an exec
        // of this process) and calls only `InheritStrip::apply` — raw syscalls
        // over integers, async-signal-safe: no allocation, no lock. `strip` is a
        // plain integer copied into the closure.
        unsafe {
            self.pre_exec(move || {
                strip.apply();
                Ok(())
            })
        }
    }
}

#[path = "spawnfd_command.rs"]
mod no_fork;
pub use no_fork::{Child, Command, Stdio};

/// A [`Command`] for `program`: its child holds its stdio and nothing else, and
/// on Darwin it is started without a fork — THE way product code starts a
/// child (`tools/grep_guard.sh` B27). `std::process::Command`'s shape:
/// arguments, environment, working directory, stdio, process group,
/// `spawn`/`output`/`status`/`exec`; [`no_fork`]'s docs list the few places it
/// differs, `pre_exec` the one it lacks.
#[must_use]
pub fn command(program: impl AsRef<OsStr>) -> Command {
    Command::new(program)
}

#[path = "spawnfd_transparent.rs"]
mod pass_through;
pub use pass_through::{TransparentChild, TransparentCommand, transparent};

/// Darwin's listing tier of [`InheritStrip::apply`].
#[cfg(target_vendor = "apple")]
mod darwin_strip {
    use super::{F_SETFD, FD_CLOEXEC, fcntl};
    use std::os::fd::RawFd;

    /// `<sys/proc_info.h>` (the same constant `holders` lists a process's
    /// descriptors with).
    const PROC_PIDLISTFDS: i32 = 1;
    /// How many descriptors one listing holds: `struct proc_fdinfo` is 8
    /// bytes, so 16 KiB of the forking thread's stack. A table with more open
    /// descriptors than this is listed up to here and probed beyond.
    pub(super) const SLOTS: usize = 2048;

    /// `struct proc_fdinfo`: the descriptor's number and its kind.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct FdInfo {
        fd: i32,
        _kind: u32,
    }

    /// The listing buffer's size in bytes, as `proc_pidinfo` takes it.
    const LISTING_BYTES: i32 = {
        let bytes = SLOTS * core::mem::size_of::<FdInfo>();
        assert!(bytes <= i32::MAX as usize);
        bytes as i32
    };

    unsafe extern "C" {
        fn getpid() -> i32;
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut core::ffi::c_void,
            size: i32,
        ) -> i32;
    }

    /// Mark every descriptor from 3 up that this process's table lists, and
    /// return the numbers the listing does NOT vouch for: none when it listed
    /// the whole table; from one past its last entry (the kernel lists in
    /// ascending order, so every open number below that was listed) to
    /// [`table_end`] when it filled the buffer; `3..bound` when it failed.
    /// `proc_pidinfo` is the `proc_info` system call and nothing more —
    /// async-signal-safe.
    pub(super) fn mark_listed(bound: RawFd) -> core::ops::Range<RawFd> {
        let mut listed = [FdInfo { fd: -1, _kind: 0 }; SLOTS];
        // SAFETY: `listed` is `LISTING_BYTES` writable bytes of `proc_fdinfo`
        // slots, which the kernel fills and does not retain; `getpid` takes
        // nothing.
        let got = unsafe {
            proc_pidinfo(
                getpid(),
                PROC_PIDLISTFDS,
                0,
                listed.as_mut_ptr().cast(),
                LISTING_BYTES,
            )
        };
        // 0 is libproc's failure answer (and a table with nothing in it).
        let Ok(bytes) = usize::try_from(got) else {
            return 3..bound;
        };
        let count = (bytes / core::mem::size_of::<FdInfo>()).min(SLOTS);
        if count == 0 {
            return 3..bound;
        }
        let mut last = -1;
        for entry in listed.iter().take(count) {
            if entry.fd >= 3 {
                // SAFETY: `F_SETFD` takes an int and touches no memory.
                unsafe {
                    fcntl(entry.fd, F_SETFD, FD_CLOEXEC);
                }
            }
            last = entry.fd;
        }
        if count < SLOTS {
            bound..bound
        } else {
            last.saturating_add(1).max(3)..table_end(bound)
        }
    }

    /// One past the highest number this process's table can hold open, at
    /// most `bound`: a listing with no buffer answers the table's size in
    /// `proc_fdinfo` slots (its allocated length plus 20 of slack — MEASURED
    /// 2026-10-01: one descriptor at 5 000 among four answered 6 420), and no
    /// open number reaches past it. A forked child's table ends just past its
    /// highest number (2 170 slots for 2 103), whatever its parent's grew to.
    /// `bound` when it cannot answer.
    fn table_end(bound: RawFd) -> RawFd {
        // SAFETY: a null buffer of size 0 asks for the size alone; nothing is
        // written. `getpid` takes nothing.
        let got = unsafe { proc_pidinfo(getpid(), PROC_PIDLISTFDS, 0, core::ptr::null_mut(), 0) };
        match usize::try_from(got) {
            Ok(bytes) if bytes > 0 => RawFd::try_from(bytes / core::mem::size_of::<FdInfo>())
                .map_or(bound, |end| end.min(bound)),
            _ => bound,
        }
    }
}

/// ONE MORE THAN THE HIGHEST DESCRIPTOR NUMBER THIS PROCESS CAN EVER HOLD, by a
/// limit no thread of an unprivileged process can raise — so a number read
/// before a fork still bounds the child's whole table, whatever another thread
/// did in between.
///
/// Darwin: `kern.maxfilesperproc` (92 160 here; root alone can change it).
/// Darwin hands out numbers up to it whenever the soft `RLIMIT_NOFILE` is
/// higher, so the soft limit — the old ceiling, capped at 65 536 — is not one.
/// Linux: the HARD `RLIMIT_NOFILE`, which only `CAP_SYS_RESOURCE` can raise
/// and which bounds every soft limit a number is allocated under — though not a
/// number opened before a process LOWERED it, which is why the strip lists the
/// table there (`close_range`, `/proc/self/fd`) and loops to this bound only
/// as its last resort. Elsewhere, or when the read fails: 2^20.
#[must_use]
pub fn descriptor_bound() -> RawFd {
    const FALLBACK: RawFd = 1 << 20;
    #[cfg(target_vendor = "apple")]
    {
        let mut value: i32 = 0;
        let mut len = std::mem::size_of::<i32>();
        // SAFETY: a NUL-terminated name and a live `i32` with its size; the
        // call writes at most `len` bytes and sets nothing.
        let rc = unsafe {
            sysctlbyname(
                c"kern.maxfilesperproc".as_ptr(),
                (&raw mut value).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 && len == std::mem::size_of::<i32>() && value > 0 {
            value
        } else {
            FALLBACK
        }
    }
    #[cfg(target_os = "linux")]
    {
        let mut limit = Rlimit { cur: 0, max: 0 };
        // SAFETY: `limit` is a live `struct rlimit` of the platform's layout,
        // which `getrlimit` fills and does not retain.
        if unsafe { getrlimit(RLIMIT_NOFILE, &mut limit) } == 0 {
            RawFd::try_from(limit.max).unwrap_or(RawFd::MAX)
        } else {
            FALLBACK
        }
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    {
        FALLBACK
    }
}

/// PASS libnotify's ONCE-GATE, so that no fork after this returns can catch it
/// half taken. Darwin; a no-op elsewhere.
///
/// `fork(2)` runs libSystem's own child handlers before it returns in the
/// child, and one of them, `_notify_fork_child`, reaches libnotify's globals
/// through an `os_alloc_once` gate. A thread INSIDE that gate — making the
/// process's first libnotify call, which `getpwuid_r`, `confstr`,
/// `os_log_create` and CFPreferences all reach — when another thread forks
/// hands the child a gate held by a thread the child does not have, and
/// libplatform kills the child inside `fork()`, before one instruction of the
/// child branch runs:
///
/// ```text
///   BUG IN CLIENT OF LIBPLATFORM: os_once_t is corrupt
///   libsystem_c.dylib: crashed on child side of fork pre-exec
///   _os_once_gate_corruption_abort <- _os_once_gate_wait <- _os_once
///     <- _os_alloc_once <- _notify_fork_child <- libSystem_atfork_child <- fork
/// ```
///
/// SIGKILL, and the parent cannot tell it from success: the child's copy of a
/// close-on-exec status pipe closes with it, which reads as an `exec` that
/// worked. Passing the gate closes the window for good — `os_once` makes a
/// caller that finds another thread inside wait for it, and a passed gate is
/// never taken again — so the forks that strip pass it themselves
/// ([`InheritStrip::prepare`]), rather than trust whatever the rest of the
/// process happened to call first. The
/// first call builds the globals (a few allocations); every later one is a
/// token lookup under libnotify's lock.
///
/// MEASURED 2026-10-01 (load 60-110 on 18 cores): 160 full runs of aterm-pty's
/// tests failed 4 times on a child lost this way — its fork-safety test's raw
/// forks and a `setsid` stand-in's `Command` — and left 5 such crash reports,
/// one of them a PTY shell forked by `spawn_shell_with_pid_cell_px`. In 30 of
/// 30 runs the process's first libnotify call was a sibling test's
/// `getpwuid_r`. The shipped binary's window passes the gate on its main
/// thread (NSApplication's `+initialize`) before it forks anything, but its
/// session forks the shell with the gate still cold — and with `$TMPDIR` unset
/// the first spawn's own status-FIFO lookup was the first caller
/// (`std::env::temp_dir` -> `confstr`, since replaced by aterm-pty's
/// `fifo_dir_in`).
///
/// libSystem's other child handlers have gates of the same shape (libxpc's
/// `os_alloc_once`, libsystem_coreservices' `os_once`); none has been seen to
/// kill a child, so none is passed here. Even `confstr`'s harm went through
/// this one alone: put back into aterm-pty's status-FIFO naming, it cost that
/// fork-safety test a child in 21 of 150 runs with this gate cold, and in 0 of
/// 150 with it passed at load.
pub fn pass_libnotify_gate() {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: a libSystem query that takes and returns plain values; on its
        // first call it builds libnotify's process globals, which is the point.
        // The answer (no token 0 was registered) is irrelevant.
        let _ = unsafe { notify_is_valid_token(0) };
    }
}

/// FOR A TEST BINARY: pass [`pass_libnotify_gate`] at IMAGE LOAD, before the
/// harness has started a thread — for a test process whose own forks (a
/// `pre_exec` stand-in, a raw `fork` probe) do not go through
/// [`InheritStrip::prepare`] while sibling tests make libnotify calls on other
/// threads. Expand it once per test crate, under
/// `#[cfg(all(test, target_vendor = "apple"))]`: dyld runs the function it
/// registers before `main`.
#[macro_export]
macro_rules! pass_libnotify_gate_at_load {
    () => {
        #[used]
        #[unsafe(link_section = "__DATA,__mod_init_func")]
        static PASS_THE_LIBNOTIFY_GATE_AT_LOAD: extern "C" fn() = {
            extern "C" fn pass() {
                $crate::spawnfd::pass_libnotify_gate();
            }
            pass
        };
    };
}

/// The Linux strip's two fast tiers. Each answers whether it marked the whole
/// table; `false` sends [`InheritStrip::apply`] on to the next.
#[cfg(target_os = "linux")]
mod linux_strip {
    use super::{F_SETFD, FD_CLOEXEC, fcntl};
    use core::ffi::{c_char, c_long};
    use std::os::fd::RawFd;

    unsafe extern "C" {
        fn syscall(number: c_long, ...) -> c_long;
        fn open(path: *const c_char, flags: i32, ...) -> RawFd;
        fn close(fd: RawFd) -> i32;
    }

    /// `close_range(3, ~0U, CLOSE_RANGE_CLOEXEC)`: Linux 5.11+. 436 on every
    /// architecture of the unified syscall table.
    pub(super) fn close_range_cloexec() -> bool {
        const SYS_CLOSE_RANGE: c_long = 436;
        const CLOSE_RANGE_CLOEXEC: c_long = 1 << 2;
        // SAFETY: three integer arguments; the kernel touches no memory of ours.
        unsafe {
            syscall(
                SYS_CLOSE_RANGE,
                3 as c_long,
                c_long::from(u32::MAX),
                CLOSE_RANGE_CLOEXEC,
            ) == 0
        }
    }

    /// Mark every number `/proc/self/fd` lists (but its own directory's), read
    /// with raw `getdents64` into a stack buffer — what CPython's
    /// `_posixsubprocess` does in the same place, for the same reason.
    pub(super) fn proc_self_fd() -> bool {
        #[cfg(target_arch = "x86_64")]
        const SYS_GETDENTS64: c_long = 217;
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        const SYS_GETDENTS64: c_long = 61;
        const O_RDONLY_CLOEXEC: i32 = 0o2_000_000;
        /// `struct linux_dirent64`: `d_reclen` at 16, `d_name` at 19.
        const RECLEN_AT: usize = 16;
        const NAME_AT: usize = 19;
        // SAFETY: a C string literal; the result is a fresh descriptor or -1.
        let dir = unsafe { open(c"/proc/self/fd".as_ptr(), O_RDONLY_CLOEXEC) };
        if dir < 0 {
            return false;
        }
        let mut buf = [0u8; 4096];
        let complete = loop {
            // SAFETY: `buf` is a live, writable buffer of the length passed.
            let n = unsafe {
                syscall(
                    SYS_GETDENTS64,
                    c_long::from(dir),
                    buf.as_mut_ptr(),
                    buf.len() as c_long,
                )
            };
            if n <= 0 {
                break n == 0;
            }
            let end = usize::try_from(n).unwrap_or(0).min(buf.len());
            let mut at = 0;
            while at + NAME_AT < end {
                let (Some(&lo), Some(&hi)) = (buf.get(at + RECLEN_AT), buf.get(at + RECLEN_AT + 1))
                else {
                    break;
                };
                let reclen = usize::from(u16::from_ne_bytes([lo, hi]));
                if reclen == 0 {
                    break;
                }
                let mut fd: RawFd = 0;
                let mut digits = 0;
                let mut i = at + NAME_AT;
                while let Some(&b) = buf.get(i) {
                    if b == 0 || i >= at + reclen {
                        break;
                    }
                    if !b.is_ascii_digit() {
                        digits = 0;
                        break;
                    }
                    fd = fd.saturating_mul(10).saturating_add(RawFd::from(b - b'0'));
                    digits += 1;
                    i += 1;
                }
                if digits > 0 && fd >= 3 && fd != dir {
                    // SAFETY: integer arguments only; an unused number is EBADF.
                    unsafe {
                        fcntl(fd, F_SETFD, FD_CLOEXEC);
                    }
                }
                at += reclen;
            }
        };
        // SAFETY: the directory descriptor opened above.
        unsafe {
            close(dir);
        }
        complete
    }
}

// The FFI every Unix build needs, in one block. `fcntl` is variadic by
// declaration, which is not cosmetic — on arm64 Darwin a variadic argument is
// passed differently from a fixed one.
unsafe extern "C" {
    fn fcntl(fd: RawFd, cmd: i32, ...) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
    #[cfg(not(target_vendor = "apple"))]
    fn access(path: *const core::ffi::c_char, mode: i32) -> i32;
    #[cfg(target_vendor = "apple")]
    fn sysctlbyname(
        name: *const core::ffi::c_char,
        old: *mut core::ffi::c_void,
        old_len: *mut usize,
        new: *mut core::ffi::c_void,
        new_len: usize,
    ) -> i32;
    /// `<notify.h>`, in libSystem: like every libnotify entry point, it builds
    /// the process's libnotify globals first ([`pass_libnotify_gate`]).
    #[cfg(target_vendor = "apple")]
    fn notify_is_valid_token(token: i32) -> bool;
    #[cfg(target_os = "linux")]
    fn getrlimit(resource: i32, limit: *mut Rlimit) -> i32;
}

/// `struct rlimit` on 64-bit Linux: two 64-bit `rlim_t`.
#[cfg(target_os = "linux")]
#[repr(C)]
struct Rlimit {
    cur: u64,
    max: u64,
}
#[cfg(target_os = "linux")]
const RLIMIT_NOFILE: i32 = 7;

// `fcntl(2)`'s commands and flag, and the errnos named here. The same numbers
// on Darwin and Linux except `F_DUPFD_CLOEXEC`.
const F_GETFD: i32 = 1;
const F_SETFD: i32 = 2;
const F_GETFL: i32 = 3;
const F_SETFL: i32 = 4;
const FD_CLOEXEC: i32 = 1;
#[cfg(target_vendor = "apple")]
const F_DUPFD_CLOEXEC: i32 = 67;
#[cfg(not(target_vendor = "apple"))]
const F_DUPFD_CLOEXEC: i32 = 1030;
const WNOHANG: i32 = 1;
const SIGKILL: i32 = 9;
#[cfg(not(target_vendor = "apple"))]
const ENOENT: i32 = 2;
#[cfg(any(test, not(target_vendor = "apple")))]
const EACCES: i32 = 13;
const EMFILE: i32 = 24;
/// `access(2)`'s execute check.
#[cfg(not(target_vendor = "apple"))]
const X_OK: i32 = 1;

#[cfg(test)]
#[path = "spawnfd_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "spawnfd_command_tests.rs"]
mod command_tests;

#[cfg(test)]
#[path = "spawnfd_transparent_tests.rs"]
mod transparent_tests;

/// TEST-ONLY, DELIBERATELY LEAKY spawns that owe nothing to `std`'s: the
/// CONTROL arm of every inheritance test here (and in [`crate::pspawn`]).
#[cfg(test)]
#[path = "spawnfd_test_support.rs"]
pub(crate) mod test_support;
