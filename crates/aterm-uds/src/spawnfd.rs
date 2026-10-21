// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Start a child that inherits exactly what it is handed (two descriptors at fixed numbers
//! plus its stdio, or its stdio alone, [`command`]), on Darwin without a fork, and, for the
//! spawns that must still fork, the strip that keeps everything else out.
//!
//! ## The lanes
//!
//! aterm launches its `aterm-link serve` bridge holding the far ends of two
//! `socketpair(AF_UNIX)`s at fds 3 and 4. The bridge's authority is the descriptor and nothing
//! else: no token file exists to steal, and the only way to be the bridge is to be the process
//! aterm spawned. `std::process::Command` can place an inherited descriptor only at 0, 1 or 2,
//! so [`spawn_with_two_fds`] takes a [`LaneCommand`] and returns a [`LaneChild`] of its own.
//!
//! A `Command` whose `pre_exec` marks every number from 3 up to a ceiling close-on-exec and
//! then `dup2`s the lanes into place fails three ways:
//!
//! 1. A ceiling capped at 65 536 misses descriptors: Darwin allocates up to
//!    `kern.maxfilesperproc` (92 160 here) whenever the soft `RLIMIT_NOFILE` is higher, and
//!    every unflagged descriptor above the ceiling reaches the bridge.
//! 2. A ceiling read in the parent before the fork races another thread that raises the limit
//!    and opens an unflagged descriptor above the old ceiling.
//! 3. `dup2` onto 3 and 4 can clobber std's own exec-status pipe, which std opens at the lowest
//!    free numbers just before its fork: a failed `exec` then reads as `Ok(child)` for a child
//!    that already died, and the error bytes go down the lane.
//!
//! ## Darwin: `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`
//!
//! The kernel marks every descriptor close-on-exec inside the spawn except those a file action
//! names: no strip, no ceiling, no window. Darwin's behaviour this rests on:
//!
//! * only a descriptor a file action names and that is not close-on-exec at the exec survives;
//! * `adddup2(fd, fd)` keeps `FD_CLOEXEC` (a source already on its target would arrive
//!   closed), and `addinherit_np(fd)` clears it;
//! * libc refuses a file action naming any number >= 10 240 (`OPEN_MAX`) with `EBADF`;
//! * the table copy is not atomic, but a concurrent change can only drop a slot from the
//!   child, never add one.
//!
//! So every source below 5 or at 10 240 and up is first moved, in the parent, to a fresh
//! `F_DUPFD_CLOEXEC` copy at 5 or above ([`off_the_targets`]). The copies are close-on-exec
//! from birth, so a concurrent `fork` elsewhere inherits none, bar an exec's:
//! `POSIX_SPAWN_SETEXEC` closes close-on-exec descriptors before its file actions run, so an
//! exec takes a flagged source from an unflagged copy held for that instant. Call `exec` from a
//! process no other thread is spawning in.
//!
//! ## Linux: our own fork, `close_range`
//!
//! Linux has no `CLOEXEC_DEFAULT`, so the child is forked here, not through `Command` (the
//! third hazard). Our own status pipe's write end sits at 5 or above like every source; the
//! child runs [`InheritStrip::apply`] and then places stdio and the lanes with `dup2`.
//! Everything it calls between `fork` and `execve` is async-signal-safe, on memory prepared
//! before the fork.
//!
//! ## [`spawn`]: the same, with no lanes
//!
//! It serves a long-lived helper this process starts and reads (`/usr/bin/log stream`), which a
//! plain `Command` on upstream std would hand every unflagged descriptor for its whole life.
//!
//! ## [`InheritStrip`]: for the spawns that must still fork
//!
//! A PTY child needs `setrlimit` and a controlling terminal (`login_tty`), neither of which
//! `posix_spawn` can express (even with `POSIX_SPAWN_SETSID`, a slave opened by a file action
//! does not become the child's terminal), so it forks and strips; so does a std command that
//! must run a `pre_exec` hook anyway ([`InheritNothing`]). The strip lists the child's own table
//! (`PROC_PIDLISTFDS` on Darwin, `close_range` or `/proc/self/fd` on Linux) and marks what it
//! finds; where a listing fails or is cut short it probes every number up to
//! [`descriptor_bound`], a limit no unprivileged thread can raise, so reading it before the
//! fork does not race as a soft-limit ceiling does. Preparing it also passes libnotify's
//! once-gate ([`pass_libnotify_gate`]), so no stripping fork loses its child in `fork()`.
//!
//! ## [`command`]: every other child, std's `Command` shape, no fork
//!
//! The window starts short-lived helpers too (`osascript`, `open`, `sysctl`, `lsof`,
//! `codesign`, an atpkg pass behind `taskpolicy`), and on upstream std a plain `Command` hands
//! each every descriptor this process holds without close-on-exec at that instant: racing
//! another thread's `UnixStream::pair()` (Darwin has no `SOCK_CLOEXEC`), a plain `Command`
//! carried a pair end into 17-27 of 200 children, [`command`] into 0 of 400. It is the one way
//! product code starts a child (the workspace clippy.toml bans `std::process::Command`; each
//! reviewed exception carries its `#[expect]`).
//!
//! [`command`] returns this module's [`Command`] (std's shape and rules; [`no_fork`]'s docs
//! list where it differs), which on Darwin is the lane spawn above: one `posix_spawn` with
//! `POSIX_SPAWN_CLOEXEC_DEFAULT`, stdio placed by file actions, the working directory by
//! `addchdir_np`, process group, session and signal defaults by spawn attributes, `exec` by
//! `POSIX_SPAWN_SETEXEC`, and the `PATH` search walked here so it reads the child's `PATH`.
//! A child asked to be its own responsible process ([`Command::disclaim_responsibility`]) gets
//! one more attribute through the private SPI [`DISCLAIM_SPI`], resolved at that spawn, and is
//! never started without it. With no fork this process is never copied, so it holds nothing
//! open in a copy (a `flock` released by closing its file stays released) and pays no bill
//! after. Off Darwin it starts through std's `Command` with [`InheritNothing`] installed first,
//! and refuses a disclaim.
//!
//! Not std's `Command` with a hook: any `pre_exec` hook moves std off `posix_spawn` onto
//! `fork`. In a window-sized process (about 6 500 VM regions, 400 MB resident;
//! `benches/spawn_cost.rs`, run by `targo --unverified bench -p aterm-uds --bench spawn_cost`)
//! the `spawn()` call took 0.13 ms for std with no hook, 2.81 ms forked with the strip and
//! 0.15 ms through [`command`]. The fork also write-protects every resident writable page, so
//! the next write to each takes a fault: rewriting those 400 MB took 22.9 ms after a forked
//! spawn against 0.4 ms after [`command`], a dropped frame per spawn.
//!
//! ## [`transparent`]: the pass-throughs, which hand on everything
//!
//! A few verbs stand in for a program their caller ran and wait for it (`aterm pkg lease --
//! <cmd>`, `aterm ship`): the caller's descriptors (a make jobserver's pipe named in
//! `MAKEFLAGS`, a `3>file` redirect) belong to that program, so its child must hold this
//! process's whole table. Trust's std (SP-1) hands a `Command` child only fds 0-2 and what the
//! unstable `CommandExt::inherit_fd` names, so [`transparent`] passes the table through
//! whatever std this binary links: on Darwin one `posix_spawn` without
//! `POSIX_SPAWN_CLOEXEC_DEFAULT`, elsewhere a fork with no strip, the stdio slots untouched. A
//! pass-through that `exec`s needs none of this: an exec keeps the table on every std.

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

/// The composing executable's startup admission of the bridge's two inherited lanes,
/// [`BRIDGE_VERB_FD`] then [`BRIDGE_PUSH_FD`], each by [`adopt_inherited_socket`].
///
/// The `aterm link` startup route takes the launcher's unclaimed lanes. A second
/// call is refused untouched; that guard does not establish the first call's
/// exclusive ownership or prevent concurrent replacement of either number.
///
/// # Safety
///
/// Each eligible lane at [`BRIDGE_VERB_FD`] and [`BRIDGE_PUSH_FD`] must have been
/// transferred exclusively to this call: no Rust or foreign owner may retain it.
/// No thread or callback may close or replace either lane during adoption. The
/// call consumes each eligible lane, including on a later failure. Missing or
/// ineligible descriptors are refused untouched.
///
/// # Errors
///
/// A second call, or [`adopt_inherited_socket`]'s, for whichever lane it refused
/// first.
pub unsafe fn adopt_startup_bridge_lanes() -> io::Result<(OwnedFd, OwnedFd)> {
    static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if TAKEN.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the bridge's inherited lanes were already taken by this process",
        ));
    }
    // SAFETY: on the GUI route these are the launcher's unclaimed lanes;
    // a hand-started process may have neither, which is a checked refusal.
    // This startup route takes each eligible descriptor once (`TAKEN`) before
    // bridge threads; neither has another Rust owner or concurrent
    // close/replacement.
    let verb = unsafe { adopt_inherited_socket(BRIDGE_VERB_FD) }?;
    // SAFETY: the same startup ownership admission for the distinct push lane.
    let push = unsafe { adopt_inherited_socket(BRIDGE_PUSH_FD) }?;
    Ok((verb, push))
}

/// Adopt an inherited socket as a close-on-exec copy, closing the original
/// number after duplication. Socket/type/flag diagnostics do not establish
/// exclusive ownership: the composing executable admits that at startup.
///
/// # Safety
///
/// The descriptor number must not be closed or replaced by another thread or
/// callback during this call. If it names a socket eligible for adoption, no
/// other owner may hold it: its ownership must have been given up to this call.
/// An absent or ineligible descriptor is allowed and is refused untouched.
/// The checks below are diagnostics, not proof of exclusive/stable ownership.
/// A refusal before the ownership take leaves `fd` open;
/// once taken, it is closed even if duplication fails.
///
/// # Errors
///
/// A closed number, a non-socket, a socket marked close-on-exec, or a failed
/// duplicate. In the first three cases the number is left untouched.
/// Close-on-exec rejects the usual accidentally presented local descriptor;
/// a cleared flag alone does not prove inheritance or lack of another owner.
pub unsafe fn adopt_inherited_socket(fd: RawFd) -> io::Result<OwnedFd> {
    use std::os::unix::fs::FileTypeExt as _;
    let meta = descriptor_metadata(fd).map_err(|e| {
        io::Error::new(
            io::ErrorKind::NotConnected,
            format!("descriptor {fd} is not open ({e})"),
        )
    })?;
    if !meta.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("descriptor {fd} is not a socket"),
        ));
    }
    // SAFETY: `F_GETFD` reads one descriptor's flags and touches no memory.
    let flags = unsafe { fcntl(fd, F_GETFD) };
    if flags < 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            format!(
                "descriptor {fd} is not open ({})",
                io::Error::last_os_error()
            ),
        ));
    }
    if flags & FD_CLOEXEC != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "descriptor {fd} is close-on-exec: this process opened it, so it is not an \
                 inherited one to adopt"
            ),
        ));
    }
    // SAFETY: the caller admitted exclusive, stable ownership; the checks
    // above only diagnose type/flags. This is the number's one owner, dropped
    // immediately after its close-on-exec copy is made.
    let inherited = unsafe { OwnedFd::from_raw_fd(fd) };
    inherited.try_clone()
}

/// What descriptor `fd` refers to, read from the descriptor itself: an `fstat` of the
/// number as it stands, never copied and never closed. `EBADF` when the number is not
/// open; any number may be asked, at any time. THE ONE READ OF A DESCRIPTOR BY NUMBER, for
/// product code and Rust tests alike, in place of a path lookup of `/dev/fd/N` (or of
/// `/proc/self/fd/N`); a shell asks with [`crate::host::sh_held_any`].
///
/// Never through a copy: Darwin kills a process that duplicates a descriptor guarded with
/// `GUARD_DUP` (`EXC_GUARD`, SIGKILL; measured 2026-10-20), and such a guard is always this
/// process's own (guarding forces close-on-exec), set by whatever opened it. `F_GETFD` and
/// `fstat` are guarded by nothing.
///
/// Never `stat("/dev/fd/N")`: Darwin's devfs has ONE vnode per number, shared by every
/// process. A `stat` of it has the descriptor's type stored in that vnode
/// (`fdesc_getattr`) and then reads it back (`vn_stat`), while any other lookup of the
/// same number, in any process or thread, resets it to `VNON` (`fdesc_allocvp`,
/// `fdesc_inactive`); a reset landing between the two answers `EBADF` for an open
/// descriptor. Measured 2026-10-20: with one other thread looking up `/dev/fd/3`, 6% of
/// path `stat`s of an open fd 3 failed so, and 0 of 80 000 `fstat`s under heavier
/// contention. `lstat` (`symlink_metadata`) and `Path::exists` race alike: with eight
/// processes looking up `/dev/fd/105`, 8 958 `stat`s and 8 880 `lstat`s of 100 000 failed,
/// while `access`, an `open` (read, write, or the shell's `>`) and a listing of `/dev/fd`
/// failed none.
///
/// # Errors
/// `EBADF` for a number that is not open.
pub fn descriptor_metadata(fd: RawFd) -> io::Result<std::fs::Metadata> {
    // SAFETY: `F_GETFD` reads one descriptor's flags and touches no memory.
    if unsafe { fcntl(fd, F_GETFD) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is open (just answered), and the `File` is never dropped
    // (`ManuallyDrop`): it names the number for one `fstat`, which changes nothing.
    // Closed by another thread in between, the `fstat` answers `EBADF`.
    let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
    file.metadata()
}

/// The lowest number a source may sit on when the spawn places it: above every
/// target (stdio 0..=2 and the two lanes).
const FIRST_FREE: RawFd = BRIDGE_PUSH_FD + 1;

/// The first number a `posix_spawn` file action may not name: Darwin's libc
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
/// It is started the way `execvp` starts a program. A bare name (no `/`) is searched on the
/// `PATH` the child will hold ([`LaneCommand::env_exactly`]'s, else this process's): the first
/// candidate that starts wins (Darwin walks them with `posix_spawn` as `posix_spawnp` does;
/// Linux takes the first regular file this process may execute, before its fork), one it may
/// not run is passed over (and reported, `EACCES`, only when nothing later matches), and an
/// unset `PATH` is `/usr/bin:/bin`. An `ENOEXEC` file (a script with no `#!`) is run by
/// `/bin/sh`.
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

    /// The child's whole environment (`env_clear` and `envs` in one call). Without this call
    /// the child gets this process's environment as it stands at the spawn.
    ///
    /// A key named twice is set once, to its last value, as `envs` does: a block carrying both
    /// would have the child's `getenv` read the first while the `PATH` search here read another.
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
/// It reaps exactly once: the first `wait`/`try_wait` that collects the exit keeps the status,
/// and later calls answer from it (a second `waitpid` could name a recycled pid). Until then
/// the pid is ours alone, so [`LaneChild::kill`] signals only our own child, and after the
/// reap nothing. Dropping an unreaped handle collects an exited child and hands a running one
/// to [`crate::reaper`], so no zombie is left either way (std leaves a running one unreaped).
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

    /// `SIGCONT` the child: a child started stopped ([`Command::start_suspended`]) runs; a
    /// running one carries on. A child already reaped is not signalled and answers `Ok`, as
    /// [`Self::kill`].
    ///
    /// # Errors
    /// `kill`'s.
    pub fn resume(&mut self) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        // SAFETY: as in `kill`: two integers, and our own unreaped child.
        if unsafe { kill(self.pid, SIGCONT) } == 0 {
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

/// A handle let go unreaped never leaves a zombie: an exited child is
/// collected here, a running one by [`crate::reaper`] once it exits.
impl Drop for LaneChild {
    fn drop(&mut self) {
        if matches!(self.try_wait(), Ok(None)) {
            crate::reaper::hand_off_pid(self.pid);
        }
    }
}

/// Spawn `cmd` with `first` and `second` inherited at [`BRIDGE_VERB_FD`] and
/// [`BRIDGE_PUSH_FD`], and nothing else above stdio.
///
/// Every descriptor handed in (the lanes, and a [`ChildStdio::Fd`]) is closed in this process
/// when the call returns, on success and failure alike: closing the parent's copies is what
/// lets it see the child's EOF when the bridge dies, so the fail-closed halt fires.
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

/// Spawn `cmd` holding its stdio and nothing else: no descriptor of this process above stdio
/// reaches it, whatever this process holds without close-on-exec (Metal's shader-cache files;
/// another thread's `socketpair` or `pipe` caught before its `F_SETFD`). [`spawn_with_two_fds`]
/// with no lanes, for a long-lived helper this process starts and reads.
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
    /// Replace this process instead of starting a child (`exec`).
    replace: bool,
    /// Where `posix_spawnp` and `execvp` part: a `PATH` entry too long to join to a bare name
    /// ends the search with `ENAMETOOLONG` (`posix_spawnp`) instead of being passed over
    /// (`execvp`). std takes `posix_spawnp` exactly when the child's `PATH` is this process's
    /// own, so [`Command`] sets it then; the lane spawns keep `execvp`'s walk.
    long_entry_ends: bool,
    /// The transparent spawn ([`transparent`]): the child inherits this process's whole table
    /// as it stands (no `POSIX_SPAWN_CLOEXEC_DEFAULT` on Darwin, no strip off it), an
    /// inherited stdio slot is left exactly as this process holds it, and the spawning
    /// thread's signal mask is kept, as upstream std spawns. `SIGPIPE` still goes back to its
    /// default.
    inherit_all: bool,
    /// THE CHILD IS ITS OWN RESPONSIBLE PROCESS: the name of the SPI that
    /// disclaims it on the spawn's attributes ([`DISCLAIM_SPI`]; a test names
    /// one that does not exist), resolved at the spawn. Darwin only, and
    /// fail-closed: a name that does not resolve, or a setter that refuses,
    /// fails the spawn before `posix_spawn`, so it never becomes a child
    /// started undisclaimed. [`Command`] alone sets it.
    disclaim: Option<&'a std::ffi::CStr>,
    /// Start the child stopped at its exec, before its first instruction
    /// (`POSIX_SPAWN_START_SUSPENDED`), for a `SIGCONT` to run, with `SIGHUP` at its
    /// default. Darwin only; [`Command`] alone sets it ([`Command::start_suspended`]).
    suspended: bool,
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
            disclaim: None,
            suspended: false,
        }
    }
}

/// `int responsibility_spawnattrs_setdisclaim(posix_spawnattr_t *attr, int
/// disclaim)`: Darwin's private SPI (libquarantine, in libSystem; no SDK
/// header declares it) that marks a spawn so the kernel makes the child its
/// OWN responsible process instead of charging it to this process's. TCC and
/// syspolicyd then ask their questions — a Developer Tools verdict, a privacy
/// grant — about the child's code identity, evaluated afresh. MEASURED
/// 2026-10-16 on this Mac: it resolves through `RTLD_DEFAULT`, answers 0, and
/// the child comes back as its own responsible process
/// (`responsibility_get_pid_responsible_for_pid`); a plain child of such a
/// process is charged to it. In a VM the same day, a process checked before
/// aterm was added to Privacy & Security ▸ Developer Tools kept a stale "not a
/// developer tool" verdict, and a fresh responsible process with aterm's
/// identity cleared it — the reason [`Command::disclaim_responsibility`]
/// exists. Named here on every OS so the refusal elsewhere is the same code
/// path's.
const DISCLAIM_SPI: &std::ffi::CStr = c"responsibility_spawnattrs_setdisclaim";

/// `fd`, or — when it sits on a target number (0..=4), or at a number a file
/// action may not name — a fresh `F_DUPFD_CLOEXEC` copy at [`FIRST_FREE`] or
/// above, the original closed.
///
/// One rule instead of a case per collision: a source on its own target would
/// be a `dup2(fd, fd)`, which keeps close-on-exec (the lane arrives closed); a
/// source on another target is clobbered by that target's placement before its
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

/// `fd` (a descriptor this process keeps across the spawn: its end of a child's pipe) or,
/// when it sits on a stdio number, a fresh copy off them ([`placeable_copy`]), the original
/// closed.
///
/// `io::pipe()` takes the lowest free numbers, so in a process that has closed a stdio slot our
/// own end lands there, and a later `Inherit` slot would hand it to the child as this
/// process's stdio (a child given the write end of its own stdin never sees EOF). Off 0..=2,
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

    // <sys/spawn.h>.
    const POSIX_SPAWN_SETPGROUP: i16 = 0x0002;
    const POSIX_SPAWN_SETSIGDEF: i16 = 0x0004;
    const POSIX_SPAWN_SETSIGMASK: i16 = 0x0008;
    const POSIX_SPAWN_SETEXEC: i16 = 0x0040;
    const POSIX_SPAWN_START_SUSPENDED: i16 = 0x0080;
    const POSIX_SPAWN_SETSID: i16 = 0x0400;
    const POSIX_SPAWN_CLOEXEC_DEFAULT: i16 = 0x4000;
    const O_RDONLY: i32 = 0;
    const O_WRONLY: i32 = 1;
    /// `sigset_t` is a 32-bit mask on Darwin, signal `n` at bit `n - 1`.
    const SIGPIPE_BIT: u32 = 1 << (13 - 1);
    /// `SIGHUP` is signal 1: bit 0.
    const SIGHUP_BIT: u32 = 1;
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
    /// `RTLD_DEFAULT`, as `<dlfcn.h>` defines it on Darwin: search every
    /// image loaded, in load order.
    const RTLD_DEFAULT: *mut c_void = (-2_isize) as *mut c_void;

    /// [`super::DISCLAIM_SPI`]'s C prototype, `int f(posix_spawnattr_t *,
    /// int)` — a `posix_spawnattr_t` being Darwin's opaque pointer, [`Attr`].
    type SetDisclaim = unsafe extern "C" fn(*mut Attr, i32) -> i32;

    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
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

    /// The disclaim setter `symbol` names ([`super::DISCLAIM_SPI`]), looked
    /// up in every loaded image — at each spawn that asks for it, never
    /// cached: a lazy static on a path the window's main thread reaches is
    /// what this repo keeps out (the 0.65/0.66 freeze), and a lookup costs
    /// microseconds beside the spawn.
    ///
    /// # Errors
    /// `Unsupported`, naming the symbol, when no loaded image exports it: the
    /// caller then starts nothing.
    fn disclaimer(symbol: &CStr) -> io::Result<SetDisclaim> {
        // SAFETY: `symbol` is a NUL-terminated C string that outlives the
        // call; `RTLD_DEFAULT` is dlfcn.h's pseudo-handle for a global search.
        let found = unsafe { dlsym(RTLD_DEFAULT, symbol.as_ptr()) };
        if found.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "this macOS exports no {}: a child asked to be its own responsible \
                     process is not started without it",
                    symbol.to_string_lossy()
                ),
            ));
        }
        // SAFETY: dyld resolved the name to a function, and the one this
        // module names, `responsibility_spawnattrs_setdisclaim`, has the C
        // prototype `int f(posix_spawnattr_t *, int)` — exactly `SetDisclaim`
        // (a test names a symbol that resolves to nothing, never another
        // function).
        Ok(unsafe { std::mem::transmute::<*mut c_void, SetDisclaim>(found) })
    }

    /// `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`: the child holds stdio and the lanes and
    /// nothing else. Every source is already at 5..10 240 (`off_the_targets`), so the actions
    /// never self-copy, clobber a source a later action reads, or name a number libc refuses.
    /// The signal mask is emptied (std leaves the spawning thread's, which in the window blocks
    /// `SIGUSR1`) and `SIGPIPE` reset to its default. `replace` turns the spawn into an `exec`
    /// of this process (`POSIX_SPAWN_SETEXEC`), which returns only on failure.
    ///
    /// `inherit_all` ([`super::transparent`]) is the one spawn without
    /// `POSIX_SPAWN_CLOEXEC_DEFAULT`: the child gets every descriptor this process holds without
    /// close-on-exec, an inherited stdio slot is named by no file action, and the spawning
    /// thread's signal mask is kept, as std does. `SIGPIPE` still goes back to its default.
    ///
    /// `disclaim` makes the child its own responsible process: the setter is resolved before the
    /// attribute object exists and called on it beside the other attributes, and either failing
    /// returns before any `posix_spawn`, so a disclaim the OS cannot apply never becomes a child
    /// started undisclaimed.
    ///
    /// `suspended` returns once the exec is done and the child stopped before its first
    /// instruction (`POSIX_SPAWN_START_SUSPENDED`); a `SIGCONT` runs it. Its `SIGHUP` goes
    /// back to the default too, whatever this process has it as: when the process holding a
    /// stopped child dies, the kernel sends the child's process group, orphaned, `SIGHUP`
    /// then `SIGCONT`, and an inherited `SIG_IGN` would let that `SIGCONT` run it (measured:
    /// a held shell whose parent ignored `SIGHUP` ran on its parent's exit; with the default
    /// it died unrun).
    pub(super) fn spawn(image: &Image, place: &Placement<'_>) -> io::Result<i32> {
        let argv = Image::pointers(&image.argv);
        let envp = Image::pointers(&image.envp);
        let disclaim = place.disclaim.map(disclaimer).transpose()?;
        let mut objects = Objects::new()?;
        let empty: u32 = 0;
        let defaults: u32 = if place.suspended {
            SIGPIPE_BIT | SIGHUP_BIT
        } else {
            SIGPIPE_BIT
        };
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
        if place.suspended {
            flags |= POSIX_SPAWN_START_SUSPENDED;
        }
        // SAFETY: each call takes an object `Objects::new` initialised and a
        // live local or C string; none retains a pointer past the call. The
        // disclaim setter is the SPI `disclaimer` resolved, called with its
        // own prototype on that same attribute object.
        unsafe {
            check(posix_spawnattr_setflags(&mut objects.attr, flags))?;
            if !place.inherit_all {
                check(posix_spawnattr_setsigmask(&mut objects.attr, &empty))?;
            }
            check(posix_spawnattr_setsigdefault(&mut objects.attr, &defaults))?;
            if let Some(pgroup) = place.pgroup {
                check(posix_spawnattr_setpgroup(&mut objects.attr, pgroup))?;
            }
            if let Some(set_disclaim) = disclaim {
                check(set_disclaim(&mut objects.attr, 1))?;
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

    /// `posix_spawnp(3)` and `execvp(3)`, as Darwin answers them: a program holding a `/` is
    /// tried once, and its error is the answer as the kernel gave it. A bare name is tried in
    /// every entry of the `PATH` the child will hold (an empty entry is the current directory;
    /// no `PATH` is `/usr/bin:/bin`): `ENOENT`, `ENOTDIR`, `ELOOP` and `ENAMETOOLONG` move on, a
    /// candidate that exists but is refused `EACCES` moves on and is the answer only if nothing
    /// later starts, and any other answer is final; nothing found is `ENOENT`. Either way
    /// `ENOEXEC` hands the file to `/bin/sh`. The two part at an entry too long to join to the
    /// name (`PATH_MAX`): `execvp` passes it over, `posix_spawnp` ends with `ENAMETOOLONG`, the
    /// walk `Placement::long_entry_ends` picks.
    ///
    /// Two differences, neither visible to a caller: a candidate that does not exist is passed
    /// over here (a `stat`) instead of costing a spawn attempt; and with a working directory
    /// set, a relative candidate is joined to it first ([`rebase`]), so it names the file the
    /// child would and `addchdir_np` never sees a relative image (with one, `posix_spawn`
    /// starts the program and still answers `ENOENT`). A `#!` script reached that way sees the
    /// joined path as `$0`.
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
            // Too long for the libc's `PATH_MAX` buffer: `execvp` passes the entry over,
            // `posix_spawnp` stops there with `ENAMETOOLONG`.
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

    /// Fork, strip, place, exec, the child calling only async-signal-safe functions on memory
    /// prepared here. Its exec status comes back on a close-on-exec pipe of our making, whose
    /// write end sits at 5 or above like every source, so no placement can clobber it: EOF is a
    /// started child, four bytes are the errno it died of before `execve` took.
    ///
    /// It places stdio and lanes only: `Command`, the one spawn that sets a working directory,
    /// a process group, a session, an `exec` or `posix_spawnp`'s walk, starts through std here
    /// (`spawnfd_command.rs`), and a disclaim (Darwin's responsibility SPI) is refused, starting
    /// nothing. `inherit_all` ([`super::transparent`]) forks with no strip. An exec refused as
    /// `ETXTBSY` is tried again ([`super::past_text_busy`]).
    pub(super) fn spawn(image: &Image, place: &Placement<'_>) -> io::Result<i32> {
        super::past_text_busy(
            super::TEXT_BUSY_FOR,
            || spawn_once(image, place),
            super::text_busy_pause,
        )
    }

    /// One fork and exec of [`spawn`].
    fn spawn_once(image: &Image, place: &Placement<'_>) -> io::Result<i32> {
        if place.cwd.is_some()
            || place.base.is_some()
            || place.pgroup.is_some()
            || place.setsid
            || place.replace
            || place.long_entry_ends
            || place.disclaim.is_some()
            || place.suspended
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

    /// The forked child. Async-signal-safe calls only (`close_range`/`fcntl`, `dup2`,
    /// `sigprocmask`, `signal`, `execve`, `write`, `_exit`), on memory the parent prepared; it
    /// allocates nothing and never returns. An `ENOEXEC` image is handed to `/bin/sh`, as
    /// `execvp` hands it. With no `strip` (the transparent spawn) it marks nothing, leaves an
    /// inherited stdio slot and the signal mask as they are, and still resets `SIGPIPE`.
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

/// The strip, for a spawn that must fork (a PTY child: `setrlimit` and a controlling terminal
/// are not `posix_spawn` file actions). [`Self::prepare`] in the parent, before the fork;
/// [`Self::apply`] in the child, between the fork and the exec: every descriptor above stdio
/// is then close-on-exec, and what the child places with `dup2` afterwards is not.
///
/// Marking rather than closing is deliberate: a status pipe the child reports
/// a failure on is already close-on-exec and must stay open until the exec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InheritStrip {
    bound: RawFd,
}

impl InheritStrip {
    /// Read [`descriptor_bound`] in the parent, before the fork, and pass libnotify's
    /// once-gate ([`pass_libnotify_gate`]), so no fork that strips (aterm-pty's spawn,
    /// [`InheritNothing`], the update hand-off's fork lane) loses its child inside `fork()`.
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

    /// Mark every descriptor from 3 up close-on-exec. Call it in a forked child before its
    /// `exec` (or right before an `exec` that replaces this process); anywhere else it would
    /// strip descriptors the process means to pass on.
    ///
    /// The descriptors are listed, not probed, wherever the kernel can list the child's own
    /// table, which nothing else can change while it runs:
    ///
    /// * Linux: `close_range(3, ~0, CLOSE_RANGE_CLOEXEC)` (5.11+), else the numbers
    ///   `/proc/self/fd` lists (read with `getdents64` into a stack buffer).
    /// * Darwin: the numbers `proc_pidinfo(PROC_PIDLISTFDS)` lists into a stack buffer of
    ///   [`darwin_strip::SLOTS`] entries. They come ascending, so a full buffer vouches for every
    ///   number up to its last entry and the loop covers only the rest, up to the end of the
    ///   table. A loop to `kern.maxfilesperproc` would cost about 8 ms on every shell spawn;
    ///   the listing costs next to nothing.
    ///
    /// The last resort, where the listing fails or is cut short: one `fcntl(F_SETFD)` per
    /// number left; an unused number is a harmless `EBADF`.
    ///
    /// Async-signal-safe by construction: raw syscalls and integer arithmetic, no allocation,
    /// no lock, no `static`, no error formatting.
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

/// A `std::process::Command` whose child inherits nothing of this process above stdio:
/// [`InheritStrip`] in a `pre_exec` hook, for a spawn that must fork anyway. Code that needs
/// nothing between the fork and the exec uses [`command`], which does not fork on Darwin.
pub trait InheritNothing {
    /// Install the strip. Hooks run in the order they were added, so install it before any
    /// hook that hands a descriptor over, or that hook's work is undone.
    ///
    /// With `exec` (which replaces this process) the strip marks this process's own table, and
    /// a failed exec does not undo it: the surviving process holds every descriptor
    /// close-on-exec, so nothing it later spawns inherits one. Use it with `exec` only where
    /// the error path hands nothing on.
    fn inherit_nothing(&mut self) -> &mut Self;
}

/// `std`'s own `Command`, under the one name a reviewed raw site TAKES it by.
///
/// The workspace bans `std::process::Command` (clippy.toml: one child
/// constructor), and this alias is that ban's one standing excuse. A fn that
/// only receives a command someone else built (a setter, an `exec`, a trait
/// impl) names it `StdCommand` and carries no `#[expect]` of its own, so no
/// fn-wide excuse also admits a raw `std::process::Command::new` in its body:
/// that construction is still linted, and the statement that does it carries
/// its own `#[expect]`. Clippy does not see through an alias, so
/// `StdCommand::new` would build a child unlinted: it is never spelled outside
/// this crate (the `aterm-census` guard).
#[expect(
    clippy::disallowed_types,
    reason = "the one name a reviewed raw std Command is taken by; constructing one stays linted at its own statement"
)]
pub type StdCommand = std::process::Command;

impl InheritNothing for StdCommand {
    fn inherit_nothing(&mut self) -> &mut Self {
        use std::os::unix::process::CommandExt as _;
        // The bound is read here, before any fork, where no thread can raise it.
        let strip = InheritStrip::prepare();
        // SAFETY: the hook runs between fork and exec (or right before an exec of this
        // process) and calls only `InheritStrip::apply`: raw syscalls over integers, no
        // allocation, no lock. `strip` is a plain integer copied into the closure.
        unsafe {
            self.pre_exec(move || {
                strip.apply();
                Ok(())
            })
        }
    }
}

/// How long a spawn the kernel refuses as `ETXTBSY` is tried again ([`past_text_busy`]),
/// and the pause between a refusal and the next try.
#[cfg(not(target_vendor = "apple"))]
const TEXT_BUSY_FOR: std::time::Duration = std::time::Duration::from_secs(1);
#[cfg(not(target_vendor = "apple"))]
const TEXT_BUSY_PAUSE: std::time::Duration = std::time::Duration::from_millis(5);

/// `spawn`, again while the kernel refuses its exec as `ETXTBSY` and `within` has not passed,
/// `pause` between a refusal and the next try; any other answer, and the last, as it came.
///
/// Linux refuses to exec a file anything holds open for writing, and a child another thread
/// forked while a file was open for writing holds that descriptor until its own exec or exit:
/// every spawn here forks (std's arm for [`Command`], the lane arm for the rest), so a program
/// this process, or any multi-threaded one, has just written — atpkg's shims and twins, a
/// test's stand-in — is refused for that window, which a loaded machine stretches (atpkg's
/// suite on the Linux lane, 2026-10-17: `Text file busy` from a shim spawned just after it was
/// laid, in 3 runs of 10 under load). A refused exec ran nothing, so trying again is safe; a writer
/// that keeps the file open is still refused after `within`. Darwin refuses no such exec.
#[cfg(not(target_vendor = "apple"))]
fn past_text_busy<T>(
    within: std::time::Duration,
    mut spawn: impl FnMut() -> io::Result<T>,
    mut pause: impl FnMut(),
) -> io::Result<T> {
    let deadline = std::time::Instant::now() + within;
    loop {
        match spawn() {
            Err(error)
                if error.kind() == io::ErrorKind::ExecutableFileBusy
                    && std::time::Instant::now() < deadline =>
            {
                pause();
            }
            done => return done,
        }
    }
}

/// A spawn this module does not make — a `std::process::Command` built with
/// [`InheritNothing`] — tried again while the kernel refuses its exec as `ETXTBSY`, as this
/// module's own are ([`past_text_busy`]): for up to a second off Darwin, once on Darwin.
///
/// # Errors
/// What `spawn` answered last.
pub fn spawn_past_text_busy<T>(spawn: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    #[cfg(not(target_vendor = "apple"))]
    {
        past_text_busy(TEXT_BUSY_FOR, spawn, text_busy_pause)
    }
    #[cfg(target_vendor = "apple")]
    {
        let mut spawn = spawn;
        spawn()
    }
}

/// The pause between an `ETXTBSY` refusal and the next try ([`TEXT_BUSY_PAUSE`]), or a test
/// thread's one-shot stand-in for it ([`TEXT_BUSY_HOOK`]).
#[cfg(not(target_vendor = "apple"))]
fn text_busy_pause() {
    #[cfg(test)]
    if let Some(hook) = TEXT_BUSY_HOOK.with(|hook| hook.borrow_mut().take()) {
        hook();
        return;
    }
    std::thread::sleep(TEXT_BUSY_PAUSE);
}

#[cfg(all(test, not(target_vendor = "apple")))]
thread_local! {
    /// What the next [`text_busy_pause`] on this test thread does instead of pausing.
    static TEXT_BUSY_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[path = "spawnfd_command.rs"]
mod no_fork;
pub use no_fork::{Child, Command, Rusage, Stdio};

/// A [`Command`] for `program`: its child holds its stdio and nothing else, and on Darwin it
/// starts without a fork. The way product code starts a child (the workspace clippy.toml bans
/// `std::process::Command`). std's shape: arguments, environment, working directory, stdio,
/// process group, `spawn`/`output`/`status`/`exec`; [`no_fork`]'s docs list where it differs,
/// `pre_exec` the one thing it lacks and [`Command::disclaim_responsibility`] the one it adds.
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

    /// Mark every descriptor from 3 up that this process's table lists, and return the numbers
    /// the listing does not vouch for: none when it listed the whole table; from one past its
    /// last entry (the kernel lists in ascending order) to [`table_end`] when it filled the
    /// buffer; `3..bound` when it failed. `proc_pidinfo` is the `proc_info` system call and
    /// nothing more: async-signal-safe.
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

    /// One past the highest number this process's table can hold open, at most `bound`: a
    /// listing with no buffer answers the table's size in `proc_fdinfo` slots (its allocated
    /// length plus 20 of slack), and no open number reaches past it. A forked child's table
    /// ends just past its highest number, whatever its parent's grew to. `bound` when it cannot
    /// answer.
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

/// One more than the highest descriptor number this process can ever hold, by a limit no
/// thread of an unprivileged process can raise, so a number read before a fork still bounds
/// the child's whole table.
///
/// Darwin: `kern.maxfilesperproc` (92 160 here; root alone can change it), which Darwin
/// allocates up to whenever the soft `RLIMIT_NOFILE` is higher, so the soft limit bounds
/// nothing. Linux: the hard `RLIMIT_NOFILE`, which only `CAP_SYS_RESOURCE` can raise, though a
/// number opened before a process lowered it can sit above it; hence the strip lists the
/// table there and loops to this bound only as its last resort. Elsewhere, or when the read
/// fails: 2^20.
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

/// Pass libnotify's once-gate, so that no fork after this returns can catch it half taken.
/// Darwin; a no-op elsewhere.
///
/// `fork(2)` runs libSystem's child handlers before it returns in the child, and
/// `_notify_fork_child` reaches libnotify's globals through an `os_alloc_once` gate. A thread
/// inside that gate (making the process's first libnotify call, which `getpwuid_r`, `confstr`,
/// `os_log_create` and CFPreferences all reach) when another thread forks hands the child a
/// gate held by a thread it does not have, and libplatform kills the child inside `fork()`:
///
/// ```text
///   BUG IN CLIENT OF LIBPLATFORM: os_once_t is corrupt
///   libsystem_c.dylib: crashed on child side of fork pre-exec
///   _os_once_gate_corruption_abort <- _os_once_gate_wait <- _os_once
///     <- _os_alloc_once <- _notify_fork_child <- libSystem_atfork_child <- fork
/// ```
///
/// The parent cannot tell that from success: the child's copy of a close-on-exec status pipe
/// closes with it, which reads as an `exec` that worked. Passing the gate closes the window for
/// good (`os_once` makes a caller that finds another thread inside wait, and a passed gate is
/// never taken again), so the forks that strip pass it themselves ([`InheritStrip::prepare`])
/// rather than trust whatever the process happened to call first. The first call builds the
/// globals (a few allocations); later ones are a token lookup under libnotify's lock.
///
/// libSystem's other child handlers have gates of the same shape (libxpc's `os_alloc_once`,
/// libsystem_coreservices' `os_once`); none has been seen to kill a child, so none is passed.
pub fn pass_libnotify_gate() {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: a libSystem query that takes and returns plain values; on its
        // first call it builds libnotify's process globals, which is the point.
        // The answer (no token 0 was registered) is irrelevant.
        let _ = unsafe { notify_is_valid_token(0) };
    }
}

/// For a test binary: pass [`pass_libnotify_gate`] at image load, before the harness starts a
/// thread, for a test process whose own forks (a `pre_exec` stand-in, a raw `fork` probe) do
/// not go through [`InheritStrip::prepare`]. Expand it once per test crate, under
/// `#[cfg(all(test, target_vendor = "apple"))]`: dyld runs the function it registers before
/// `main`.
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

// The FFI every Unix build needs. `fcntl` is variadic by declaration, which is not cosmetic:
// on arm64 Darwin a variadic argument is passed differently from a fixed one.
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
/// `SIGCONT`: 19 on Darwin and the BSDs, 18 on Linux.
#[cfg(target_vendor = "apple")]
const SIGCONT: i32 = 19;
#[cfg(not(target_vendor = "apple"))]
const SIGCONT: i32 = 18;
#[cfg(not(target_vendor = "apple"))]
const ENOENT: i32 = 2;
#[cfg(any(test, not(target_vendor = "apple")))]
const EACCES: i32 = 13;
const EMFILE: i32 = 24;
/// `access(2)`'s execute check.
#[cfg(not(target_vendor = "apple"))]
const X_OK: i32 = 1;

/// `spawnfd`'s tests; [`tests::translator`] is every test's in the crate.
#[cfg(test)]
#[path = "spawnfd_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "spawnfd_command_tests.rs"]
mod command_tests;

#[cfg(test)]
#[path = "spawnfd_transparent_tests.rs"]
mod transparent_tests;

/// Test-only, deliberately leaky spawns that owe nothing to std's: the control arm of every
/// inheritance test here (and in [`crate::pspawn`]).
#[cfg(test)]
#[path = "spawnfd_test_support.rs"]
pub(crate) mod test_support;
