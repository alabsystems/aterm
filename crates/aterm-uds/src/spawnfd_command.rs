// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`Command`], [`Stdio`] and [`Child`]: `std::process`'s shapes for a child that holds its
//! stdio and nothing else of this process, started on Darwin by one `posix_spawn` with
//! `POSIX_SPAWN_CLOEXEC_DEFAULT`, so this process never forks for it. [`super::command`] is
//! the constructor product code uses; the parent module's docs say why.
//!
//! It is `std::process::Command` where a caller can tell, and differs only where std would hand
//! the child more than its stdio:
//!
//! * Arguments, environment, working directory: std's rules. `env`, `envs`, `env_remove` and
//!   `env_clear` edit a copy of this process's environment taken at the spawn (a changed one
//!   is passed sorted by key, as std passes it); a NUL anywhere is `InvalidInput`.
//! * The program: `execvp`'s search. A name holding a `/` is that file (relative to the
//!   child's working directory); a bare name is searched on the `PATH` the child will hold
//!   (`/usr/bin:/bin` when it holds none); a candidate that may not be run (`EACCES`) is passed
//!   over and reported only when nothing later starts; an `ENOEXEC` file is run by `/bin/sh`.
//!   A `PATH` entry too long to join to the name ends the search with `ENAMETOOLONG` when the
//!   child's `PATH` is this process's own (std then takes `posix_spawnp`) and is passed over
//!   when it was edited (std then forks and takes `execvp`). Failures carry std's errno:
//!   `NotFound` for a missing program or working directory, `PermissionDenied` for one that may
//!   not be run. A failed exec is an `Err`, never a child.
//! * Stdio: std's defaults (`spawn` and `status` inherit all three; `output` reads `/dev/null`
//!   and pipes stdout and stderr) and modes: [`Stdio::inherit`], [`Stdio::null`],
//!   [`Stdio::piped`] (the parent's end comes back as std's own `ChildStdin` / `ChildStdout` /
//!   `ChildStderr`), or a descriptor placed at that number. One difference, on Darwin: an
//!   inherited slot this process has closed is given `/dev/null`, since a child born with
//!   stdout closed hands its first `open` that number (off Darwin std's rule stands). This
//!   process's end of a pipe never sits on 0..=2, where it would pass for an inherited slot.
//! * Signals: `SIGPIPE` back to its default, the others as this process has them (both as
//!   std), and `SIGHUP` back to its default for a child started stopped
//!   ([`Command::start_suspended`]); the signal mask starts empty, where std copies the
//!   spawning thread's, which in the window blocks `SIGUSR1` (aterm-gui's snapshot thread).
//! * Process group and session: [`Command::process_group`] and [`Command::setsid`], spawn
//!   attributes on Darwin; with both, the session wins and the child leads its own group.
//! * Responsibility, which std has no word for: [`Command::disclaim_responsibility`] starts the
//!   child as its own responsible process (Darwin's private spawn attribute), so macOS asks its
//!   TCC and syspolicyd questions about the child's code identity instead of this process's
//!   responsible app. Fail-closed: off Darwin, where the SPI is missing, and beside `exec` it is
//!   `Unsupported` and nothing is started.
//! * A child started stopped at its exec ([`Command::start_suspended`], Darwin's spawn flag),
//!   for [`Child::resume`] to run; std has no word for it either. Off Darwin and beside `exec`
//!   it is `Unsupported` and nothing is started.
//! * No `pre_exec`: code between a fork and an exec is what this type exists to avoid. A
//!   spawn that needs it (a PTY child's `setrlimit` and controlling terminal, a descriptor
//!   handed over above stdio) builds a `std::process::Command` with
//!   [`super::InheritNothing`] and is a reviewed exception.
//!
//! [`Child`] reaps exactly once: the first `wait`/`try_wait` that collects the exit keeps it
//! and later calls answer from it; `kill` after the reap signals nothing (the pid may name
//! another process by then) and answers `Ok`, and `wait` closes the child's stdin first, both
//! as std does; dropping an unreaped handle collects an exited child and leaves a running one
//! to [`crate::reaper`] (std leaves it unreaped).
//!
//! Off Darwin the same type starts its child through std, forked, with
//! [`super::InheritStrip`] in the child and the mask emptied there too.

use super::{DISCLAIM_SPI, F_GETFL, F_SETFL, fcntl};
use std::collections::BTreeMap;
use std::ffi::{CStr, OsStr, OsString};
use std::fmt;
use std::io::{self, Read as _};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdin, ChildStdout, ExitStatus, Output};

/// What a child's stdin, stdout or stderr is: `std::process::Stdio`'s modes.
pub struct Stdio(Kind);

#[derive(Debug)]
enum Kind {
    Inherit,
    Null,
    Piped,
    Fd(OwnedFd),
}

impl Stdio {
    /// A new pipe: the child holds one end, [`Child`] the other.
    #[must_use]
    pub fn piped() -> Self {
        Self(Kind::Piped)
    }

    /// This process's own descriptor at that number.
    #[must_use]
    pub fn inherit() -> Self {
        Self(Kind::Inherit)
    }

    /// `/dev/null`.
    #[must_use]
    pub fn null() -> Self {
        Self(Kind::Null)
    }
}

impl fmt::Debug for Stdio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Stdio").field(&self.0).finish()
    }
}

impl From<OwnedFd> for Stdio {
    /// This descriptor, placed at the slot's number. The command keeps it
    /// (open in this process) until the command is dropped, as `std` keeps a
    /// `Stdio::from(fd)`.
    fn from(fd: OwnedFd) -> Self {
        Self(Kind::Fd(fd))
    }
}

impl From<std::fs::File> for Stdio {
    fn from(file: std::fs::File) -> Self {
        Self(Kind::Fd(file.into()))
    }
}

impl From<ChildStdin> for Stdio {
    fn from(pipe: ChildStdin) -> Self {
        Self(Kind::Fd(pipe.into()))
    }
}

impl From<ChildStdout> for Stdio {
    fn from(pipe: ChildStdout) -> Self {
        Self(Kind::Fd(pipe.into()))
    }
}

impl From<ChildStderr> for Stdio {
    fn from(pipe: ChildStderr) -> Self {
        Self(Kind::Fd(pipe.into()))
    }
}

impl From<io::PipeReader> for Stdio {
    fn from(pipe: io::PipeReader) -> Self {
        Self(Kind::Fd(pipe.into()))
    }
}

impl From<io::PipeWriter> for Stdio {
    fn from(pipe: io::PipeWriter) -> Self {
        Self(Kind::Fd(pipe.into()))
    }
}

/// `std`'s `CommandEnv`: the edits a command makes to the environment it
/// will copy at the spawn.
#[derive(Debug, Default)]
struct Env {
    clear: bool,
    vars: BTreeMap<OsString, Option<OsString>>,
}

impl Env {
    fn set(&mut self, key: &OsStr, value: &OsStr) {
        self.vars.insert(key.to_owned(), Some(value.to_owned()));
    }

    fn remove(&mut self, key: &OsStr) {
        if self.clear {
            self.vars.remove(key);
        } else {
            self.vars.insert(key.to_owned(), None);
        }
    }

    fn clear(&mut self) {
        self.clear = true;
        self.vars.clear();
    }

    /// Whether the child's `PATH` may differ from this process's: std's `have_changed_path`,
    /// the test that sends a bare name from `posix_spawnp` (which searches this process's
    /// `PATH`) to `execvp`.
    #[cfg(target_vendor = "apple")]
    fn saw_path(&self) -> bool {
        self.clear || self.vars.contains_key(OsStr::new("PATH"))
    }

    /// The child's whole environment: this process's as it stands (in
    /// `environ`'s order) when nothing was edited, else this process's (or
    /// none, after `env_clear`) with the edits applied, sorted by key —
    /// exactly the block `std` hands the child.
    #[cfg(target_vendor = "apple")]
    fn capture(&self) -> Vec<(OsString, OsString)> {
        if !self.clear && self.vars.is_empty() {
            return std::env::vars_os().collect();
        }
        let mut env = BTreeMap::new();
        if !self.clear {
            env.extend(std::env::vars_os());
        }
        for (key, value) in &self.vars {
            match value {
                Some(value) => {
                    env.insert(key.clone(), value.clone());
                }
                None => {
                    env.remove(key);
                }
            }
        }
        env.into_iter().collect()
    }
}

/// `std::process::Command`, whose child holds its stdio and nothing else
/// (the module docs list where it differs). Built by [`super::command`].
pub struct Command {
    program: OsString,
    arg0: Option<OsString>,
    args: Vec<OsString>,
    env: Env,
    cwd: Option<PathBuf>,
    stdin: Option<Stdio>,
    stdout: Option<Stdio>,
    stderr: Option<Stdio>,
    pgroup: Option<i32>,
    setsid: bool,
    /// The SPI that makes the child its own responsible process, when asked
    /// ([`Self::disclaim_responsibility`]): always [`DISCLAIM_SPI`] but in a
    /// test that names one no image exports.
    disclaim: Option<&'static CStr>,
    /// Start the child stopped at its exec ([`Self::start_suspended`]).
    suspended: bool,
}

/// The stdio a spawn gives a slot the caller left unset.
#[derive(Clone, Copy)]
enum Fallback {
    Inherit,
    Piped,
}

impl Command {
    /// `program` with no arguments, this process's environment and working
    /// directory, and stdio inherited (`spawn`/`status`) or piped (`output`).
    #[must_use]
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            arg0: None,
            args: Vec::new(),
            env: Env::default(),
            cwd: None,
            stdin: None,
            stdout: None,
            stderr: None,
            pgroup: None,
            setsid: false,
            disclaim: None,
            suspended: false,
        }
    }

    /// The child's `argv[0]`, in place of the program as it was named (which
    /// is still what is searched for and run) —
    /// `std::os::unix::process::CommandExt::arg0`'s.
    pub fn arg0(&mut self, arg0: impl AsRef<OsStr>) -> &mut Self {
        self.arg0 = Some(arg0.as_ref().to_owned());
        self
    }

    /// Append one argument.
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    /// Append arguments.
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        self
    }

    /// Set one variable in the child's environment.
    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.env.set(key.as_ref(), value.as_ref());
        self
    }

    /// Set variables in the child's environment.
    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (key, value) in vars {
            self.env.set(key.as_ref(), value.as_ref());
        }
        self
    }

    /// Leave one variable out of the child's environment.
    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.env.remove(key.as_ref());
        self
    }

    /// Start the child's environment empty: only what `env`/`envs` set after
    /// this reaches it.
    pub fn env_clear(&mut self) -> &mut Self {
        self.env.clear();
        self
    }

    /// The child's working directory. A relative program or `PATH` entry is
    /// resolved against it, as `execvp` after a `chdir` resolves it.
    pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
        self.cwd = Some(dir.as_ref().to_owned());
        self
    }

    /// The child's stdin.
    pub fn stdin(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.stdin = Some(cfg.into());
        self
    }

    /// The child's stdout.
    pub fn stdout(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.stdout = Some(cfg.into());
        self
    }

    /// The child's stderr.
    pub fn stderr(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.stderr = Some(cfg.into());
        self
    }

    /// Start the child in process group `pgroup` (`0`: a new group of its
    /// own, led by the child) — `std::os::unix::process::CommandExt`'s.
    /// Beside [`Self::setsid`] the session wins, as the kernel orders the two
    /// (`setpgid`, then `setsid`): the child leads a group of its own whatever
    /// group was asked, and `0` — which would make it a group leader that
    /// `setsid` then refuses — is not asked of the kernel at all.
    pub fn process_group(&mut self, pgroup: i32) -> &mut Self {
        self.pgroup = Some(pgroup);
        self
    }

    /// Start the child in a new session of its own (`setsid`), with no
    /// controlling terminal. The child then leads its own process group too,
    /// so [`Self::process_group`]`(0)` beside it changes nothing.
    pub fn setsid(&mut self, setsid: bool) -> &mut Self {
        self.setsid = setsid;
        self
    }

    /// Start the child as its OWN RESPONSIBLE PROCESS (`true`), not charged to
    /// the one this process is charged to. macOS attributes a process's TCC
    /// and syspolicyd questions — a privacy grant, the Developer Tools verdict
    /// — to its responsible process, which a child inherits from a parent that
    /// has one (a shell's child under an app is asked about as that app).
    /// Disclaimed, the child is asked about as its own code identity, and
    /// evaluated afresh (aterm starts its own executable this way once the
    /// owner adds aterm to Privacy & Security ▸ Developer Tools: a process
    /// checked before the grant keeps a stale "not a developer tool" verdict,
    /// a fresh responsible process with aterm's identity clears it — measured
    /// in a VM 2026-10-16). A plain child of a disclaimed one is charged to it.
    ///
    /// Darwin's private SPI `responsibility_spawnattrs_setdisclaim`, looked up
    /// at each spawn and set on the spawn's attributes. FAIL-CLOSED: a child
    /// asked to disclaim is never started without it. The spawn answers
    /// `Unsupported`, starting nothing, off Darwin, on a macOS whose libSystem
    /// exports no such SPI, and beside [`Self::exec`] (`POSIX_SPAWN_SETEXEC`
    /// with a disclaim is unmeasured); a refusal by the SPI itself is the
    /// spawn's error, likewise before anything starts. `false` (the default)
    /// asks for nothing.
    pub fn disclaim_responsibility(&mut self, disclaim: bool) -> &mut Self {
        self.disclaim = disclaim.then_some(DISCLAIM_SPI);
        self
    }

    /// Start the child STOPPED AT ITS EXEC (`true`): `spawn` returns once the kernel has
    /// exec'd the program — its image mapped and its code signature checked — and not one
    /// instruction of the program, nor of the dynamic loader before it, runs until
    /// [`Child::resume`]. What the system does at a binary's first exec, syspolicyd's
    /// assessment included, goes on while it is stopped (measured on the 96 MB `aterm`: the
    /// spawn returned in 9.7 ms; resumed at once it exited 898 ms later, resumed after
    /// 1.2 s stopped 18 ms later). One killed before its resume never runs; one dropped
    /// neither resumed nor killed stays stopped until its holder exits
    /// ([`crate::deadline::Held`] kills it on drop).
    ///
    /// ITS HOLDER'S DEATH KILLS IT UNRUN, in a process group of its own
    /// ([`Self::process_group`]`(0)`): the kernel sends a stopped group orphaned by its
    /// holder's exit `SIGHUP` then `SIGCONT`, and the child starts with `SIGHUP` at its
    /// default whatever this process has it as, so the hangup ends it before it runs (an
    /// inherited `SIG_IGN` would let the `SIGCONT` run it).
    ///
    /// Darwin's `POSIX_SPAWN_START_SUSPENDED`. FAIL-CLOSED like
    /// [`Self::disclaim_responsibility`]: off Darwin (where no spawn can stop a child after
    /// its exec) and beside [`Self::exec`] the spawn answers `Unsupported` and starts
    /// nothing; it never starts the child running instead. `false` (the default) asks for
    /// nothing.
    pub fn start_suspended(&mut self, suspended: bool) -> &mut Self {
        self.suspended = suspended;
        self
    }

    /// TEST SEAM: disclaim through the SPI named `spi` instead of
    /// [`DISCLAIM_SPI`] — one no image exports, to drive the fail-closed path.
    #[cfg(test)]
    pub(super) fn disclaim_through(&mut self, spi: &'static CStr) -> &mut Self {
        self.disclaim = Some(spi);
        self
    }

    /// The group the spawn asks for: none beside `setsid` when the group
    /// asked is `0`, which the session gives anyway. Asked as well, `setpgid`
    /// makes the child a group leader and `setsid` then refuses it: the spawn
    /// failed `EPERM` (Darwin's `posix_spawn` and `std`'s fork alike).
    fn group(&self) -> Option<i32> {
        self.pgroup.filter(|&pgroup| !(self.setsid && pgroup == 0))
    }

    /// The program, as it was named.
    #[must_use]
    pub fn get_program(&self) -> &OsStr {
        &self.program
    }

    /// The arguments after the program.
    pub fn get_args(&self) -> impl ExactSizeIterator<Item = &OsStr> {
        self.args.iter().map(OsString::as_os_str)
    }

    /// The environment edits, sorted by key: `None` for a removed variable.
    /// (After `env_clear`, the variables set since.)
    pub fn get_envs(&self) -> impl ExactSizeIterator<Item = (&OsStr, Option<&OsStr>)> {
        self.env
            .vars
            .iter()
            .map(|(key, value)| (key.as_os_str(), value.as_deref()))
    }

    /// The working directory set by [`Self::current_dir`].
    #[must_use]
    pub fn get_current_dir(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Start the child; unset stdio is inherited.
    ///
    /// # Errors
    /// As `std`'s: `InvalidInput` for a NUL, `NotFound` for a program (or a
    /// working directory) that does not exist, `PermissionDenied` for one
    /// that may not be run, and whatever else the spawn answers.
    pub fn spawn(&mut self) -> io::Result<Child> {
        self.start(Fallback::Inherit, true)
    }

    /// Run the child to its end with stdin `/dev/null` and stdout and stderr
    /// captured (unless set otherwise).
    ///
    /// # Errors
    /// As [`Self::spawn`]'s, and reading or waiting.
    pub fn output(&mut self) -> io::Result<Output> {
        self.start(Fallback::Piped, false)?.wait_with_output()
    }

    /// Run the child to its end with unset stdio inherited.
    ///
    /// # Errors
    /// As [`Self::spawn`]'s, and waiting.
    pub fn status(&mut self) -> io::Result<ExitStatus> {
        self.start(Fallback::Inherit, true)?.wait()
    }
}

impl fmt::Debug for Command {
    /// `std`'s shape: `cd "dir" && env -i K="v" "program" "arg"`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(cwd) = &self.cwd {
            write!(f, "cd {cwd:?} && ")?;
        }
        if self.env.clear {
            write!(f, "env -i ")?;
        } else if self.env.vars.values().any(Option::is_none) {
            write!(f, "env ")?;
            for (key, _) in self.env.vars.iter().filter(|(_, v)| v.is_none()) {
                write!(f, "-u {} ", key.to_string_lossy())?;
            }
        }
        for (key, value) in &self.env.vars {
            if let Some(value) = value {
                write!(f, "{}={value:?} ", key.to_string_lossy())?;
            }
        }
        match &self.arg0 {
            Some(arg0) if *arg0 != self.program => write!(f, "[{:?}] {arg0:?}", self.program)?,
            _ => write!(f, "{:?}", self.program)?,
        }
        for arg in &self.args {
            write!(f, " {arg:?}")?;
        }
        Ok(())
    }
}

/// A child [`Command`] started: `std::process::Child`'s shape, with `std`'s
/// own pipe types in the three public fields.
pub struct Child {
    /// The parent's end of a piped stdin.
    pub stdin: Option<ChildStdin>,
    /// The parent's end of a piped stdout.
    pub stdout: Option<ChildStdout>,
    /// The parent's end of a piped stderr.
    pub stderr: Option<ChildStderr>,
    handle: Handle,
    usage: Option<Rusage>,
}

/// A child's exit status and the resources it and the descendants it reaped used.
/// Collected with `wait4` on Darwin and Linux, under the child's own reaping handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rusage {
    pub status: ExitStatus,
    /// User plus system CPU time.
    pub cpu: std::time::Duration,
    /// The largest process's resident peak, in bytes on every host.
    pub maxrss: u64,
}

#[path = "spawnfd_rusage.rs"]
mod rusage;

impl fmt::Debug for Child {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Child")
            .field("pid", &self.id())
            .field("stdin", &self.stdin)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish_non_exhaustive()
    }
}

impl Child {
    /// Wait for the child and collect its resource accounting. Repeated calls return
    /// the same result; ordinary `wait` and `try_wait` then share its cached status.
    /// Call this before reaping through either ordinary method: resource accounting
    /// cannot be recovered after another wait collected the child.
    ///
    /// # Errors
    /// `wait4`'s, or `Unsupported` off 64-bit Darwin/Linux. An already reaped child whose
    /// accounting was not collected returns an error without asking its old pid.
    pub fn wait_with_rusage(&mut self) -> io::Result<Rusage> {
        drop(self.stdin.take());
        self.collect_rusage(true)?
            .ok_or_else(|| io::Error::other("wait4 returned no exit status"))
    }

    /// Collect status and resource accounting if the child has exited, else `None`.
    /// The same ownership and caching rules as [`Self::wait_with_rusage`].
    ///
    /// # Errors
    /// As [`Self::wait_with_rusage`].
    pub fn try_wait_with_rusage(&mut self) -> io::Result<Option<Rusage>> {
        self.collect_rusage(false)
    }

    fn collect_rusage(&mut self, block: bool) -> io::Result<Option<Rusage>> {
        if let Some(usage) = self.usage {
            return Ok(Some(usage));
        }
        if self.handle.status.is_some() {
            return Err(io::Error::other(
                "child already reaped without resource accounting",
            ));
        }
        let usage = rusage::wait(self.id(), block)?;
        if let Some(usage) = usage {
            self.handle.status = Some(usage.status);
            self.usage = Some(usage);
        }
        Ok(usage)
    }

    /// The child's pid.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.handle.id()
    }

    /// `SIGKILL` the child — never once it is reaped (the module docs).
    ///
    /// # Errors
    /// `kill`'s.
    pub fn kill(&mut self) -> io::Result<()> {
        self.handle.kill()
    }

    /// Run a child started stopped ([`Command::start_suspended`]): `SIGCONT`, which a
    /// running child carries on through. Never once it is reaped, as [`Self::kill`].
    ///
    /// # Errors
    /// `kill`'s.
    pub fn resume(&mut self) -> io::Result<()> {
        self.handle.resume()
    }

    /// Close the child's stdin, then wait for it to exit and reap it (once).
    ///
    /// # Errors
    /// `waitpid`'s, other than `EINTR` (retried).
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        drop(self.stdin.take());
        self.handle.wait()
    }

    /// The exit status if the child has exited (reaping it), else `None`.
    ///
    /// # Errors
    /// `waitpid`'s, other than `EINTR` (retried).
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.handle.try_wait()
    }

    /// Close the child's stdin, read its piped stdout and stderr to their
    /// ends (both at once, so neither can fill and stall the child), and
    /// wait for it.
    ///
    /// # Errors
    /// Reading's, or waiting's.
    pub fn wait_with_output(mut self) -> io::Result<Output> {
        drop(self.stdin.take());
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        match (self.stdout.take(), self.stderr.take()) {
            (None, None) => {}
            (Some(mut out), None) => {
                out.read_to_end(&mut stdout)?;
            }
            (None, Some(mut err)) => {
                err.read_to_end(&mut stderr)?;
            }
            (Some(out), Some(err)) => read2(out, &mut stdout, err, &mut stderr)?,
        }
        let status = self.wait()?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

/// Read two pipes to their ends at once: `poll` both, read whichever is
/// ready, and once one ends read the other to its end — `std`'s
/// `read_output`.
fn read2(
    mut out: ChildStdout,
    out_buf: &mut Vec<u8>,
    mut err: ChildStderr,
    err_buf: &mut Vec<u8>,
) -> io::Result<()> {
    #[repr(C)]
    struct PollFd {
        fd: RawFd,
        events: i16,
        revents: i16,
    }
    #[cfg(target_vendor = "apple")]
    type Nfds = core::ffi::c_uint;
    #[cfg(not(target_vendor = "apple"))]
    type Nfds = core::ffi::c_ulong;
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: Nfds, timeout: i32) -> i32;
    }
    const POLLIN: i16 = 0x0001;
    set_nonblocking(out.as_raw_fd(), true)?;
    set_nonblocking(err.as_raw_fd(), true)?;
    let mut fds = [
        PollFd {
            fd: out.as_raw_fd(),
            events: POLLIN,
            revents: 0,
        },
        PollFd {
            fd: err.as_raw_fd(),
            events: POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: `fds` is a live array of two `struct pollfd`, which `poll`
        // reads and writes and does not retain.
        if unsafe { poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if fds[0].revents != 0 && read_ready(&mut out, out_buf)? {
            set_nonblocking(err.as_raw_fd(), false)?;
            return err.read_to_end(err_buf).map(drop);
        }
        if fds[1].revents != 0 && read_ready(&mut err, err_buf)? {
            set_nonblocking(out.as_raw_fd(), false)?;
            return out.read_to_end(out_buf).map(drop);
        }
    }
}

/// Read what a nonblocking pipe holds: `true` once it has ended.
fn read_ready(pipe: &mut impl io::Read, buf: &mut Vec<u8>) -> io::Result<bool> {
    match pipe.read_to_end(buf) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(false),
        Err(e) => Err(e),
    }
}

fn set_nonblocking(fd: RawFd, on: bool) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    const O_NONBLOCK: i32 = 0x0004;
    #[cfg(not(target_vendor = "apple"))]
    const O_NONBLOCK: i32 = 0o4000;
    // SAFETY: `F_GETFL`/`F_SETFL` read and set one descriptor's status flags
    // and touch no memory; `fd` is a pipe end owned by the caller.
    unsafe {
        let flags = fcntl(fd, F_GETFL);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let flags = if on {
            flags | O_NONBLOCK
        } else {
            flags & !O_NONBLOCK
        };
        if fcntl(fd, F_SETFL, flags) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Darwin: posix_spawn, CLOEXEC_DEFAULT
// ---------------------------------------------------------------------------

#[cfg(target_vendor = "apple")]
type Handle = super::LaneChild;

#[cfg(target_vendor = "apple")]
impl Command {
    /// Replace this process with the program (`POSIX_SPAWN_SETEXEC`): it holds this process's
    /// stdio and nothing else, and this process's own table is untouched if the exec fails.
    /// Unset stdio is inherited. Returns only on failure — at once, `Unsupported` and this
    /// process untouched, beside [`Self::disclaim_responsibility`].
    pub fn exec(&mut self) -> io::Error {
        match self.launch(Fallback::Inherit, true, true) {
            Ok(_) => io::Error::other("the exec returned a child"),
            Err(error) => error,
        }
    }

    fn start(&mut self, fallback: Fallback, needs_stdin: bool) -> io::Result<Child> {
        let (pid, [stdin, stdout, stderr]) = self.launch(fallback, needs_stdin, false)?;
        Ok(Child {
            stdin: stdin.map(ChildStdin::from),
            stdout: stdout.map(ChildStdout::from),
            stderr: stderr.map(ChildStderr::from),
            handle: super::LaneChild { pid, status: None },
            usage: None,
        })
    }

    /// The spawn: the child's pid, and this process's ends of its pipes.
    /// Every descriptor the child is handed (a pipe's far end, a copy moved
    /// above the targets) is closed here when it returns, either way.
    fn launch(
        &mut self,
        fallback: Fallback,
        needs_stdin: bool,
        replace: bool,
    ) -> io::Result<(i32, [Option<OwnedFd>; 3])> {
        use super::{
            F_GETFD, F_SETFD, FD_CLOEXEC, Image, Placement, Source, c_string, fcntl, placeable,
            placeable_copy,
        };
        use std::os::unix::ffi::OsStrExt as _;
        // An exec that disclaims would make THIS process, replaced, its own responsible
        // process: `POSIX_SPAWN_SETEXEC` with the disclaim attribute was never measured, so it
        // is refused before anything is built, never tried.
        if replace && self.disclaim.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "an exec cannot disclaim responsibility: spawn the disclaimed child instead",
            ));
        }
        // An exec started stopped would stop THIS process, replaced, with nobody holding it
        // to resume: refused before anything is built.
        if replace && self.suspended {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "an exec cannot start suspended: spawn the child instead",
            ));
        }
        let mut image = Image::build(&self.program, &self.args, &self.env.capture())?;
        if let Some(arg0) = &self.arg0 {
            image.argv[0] = c_string(arg0.as_bytes())?;
        }
        let cwd = self
            .cwd
            .as_ref()
            .map(|dir| c_string(dir.as_os_str().as_bytes()))
            .transpose()?;
        // The directory a relative program or `PATH` entry resolves against
        // in the child. Without this process's own directory a relative one
        // cannot be made absolute; it is then passed as it is.
        let base = match &self.cwd {
            Some(dir) if dir.is_absolute() => Some(dir.clone()),
            Some(dir) => std::env::current_dir().ok().map(|here| here.join(dir)),
            None => None,
        };
        let mut handed: Vec<OwnedFd> = Vec::new();
        let mut ours: [Option<OwnedFd>; 3] = [None, None, None];
        let mut sources = [Source::Inherit; 3];
        for (target, slot) in [&self.stdin, &self.stdout, &self.stderr]
            .into_iter()
            .enumerate()
        {
            let kind = match slot {
                Some(Stdio(kind)) => kind,
                None if target == 0 && !needs_stdin => &Kind::Null,
                None => match fallback {
                    Fallback::Inherit => &Kind::Inherit,
                    Fallback::Piped => &Kind::Piped,
                },
            };
            sources[target] = match kind {
                Kind::Inherit => Source::Inherit,
                Kind::Null => Source::Null,
                Kind::Piped => {
                    let (read, write) = io::pipe()?;
                    let (mine, theirs) = if target == 0 {
                        (OwnedFd::from(write), OwnedFd::from(read))
                    } else {
                        (OwnedFd::from(read), OwnedFd::from(write))
                    };
                    let theirs = super::off_the_targets(theirs)?;
                    let source = Source::Fd(theirs.as_raw_fd());
                    handed.push(theirs);
                    ours[target] = Some(super::off_stdio(mine)?);
                    source
                }
                Kind::Fd(fd) if placeable(fd.as_raw_fd()) => Source::Fd(fd.as_raw_fd()),
                Kind::Fd(fd) => {
                    let copy = placeable_copy(fd.as_raw_fd())?;
                    let source = Source::Fd(copy.as_raw_fd());
                    handed.push(copy);
                    source
                }
            };
        }
        if replace {
            // `POSIX_SPAWN_SETEXEC` closes this process's close-on-exec descriptors before it
            // runs the file actions, so a `dup2` or an inherit from one answers `EBADF`, and
            // every descriptor std opens carries the flag. The exec takes each such source from
            // a copy without it: `CLOEXEC_DEFAULT` still keeps the copy itself out of the new
            // image, and a failed exec closes it with the rest of `handed`. A fork on another
            // thread in that instant would inherit the copy: exec from a quiescent process (the
            // keeper's re-exec is one). An unflagged source is inherited as it is.
            for (target, source) in (0..).zip(sources.iter_mut()) {
                let fd = match *source {
                    Source::Fd(fd) => fd,
                    Source::Inherit => target,
                    Source::Null => continue,
                };
                // SAFETY: `F_GETFD` reads one descriptor's flags; a closed
                // number answers -1 (an inherited slot this process has closed,
                // which the spawn gives `/dev/null`).
                let flags = unsafe { fcntl(fd, F_GETFD) };
                if flags < 0 || flags & FD_CLOEXEC == 0 {
                    continue;
                }
                let copy = placeable_copy(fd)?;
                // SAFETY: `F_SETFD` clears close-on-exec on the copy made here.
                if unsafe { fcntl(copy.as_raw_fd(), F_SETFD, 0) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                *source = Source::Fd(copy.as_raw_fd());
                handed.push(copy);
            }
        }
        let pid = super::platform::spawn(
            &image,
            &Placement {
                stdio: sources,
                lanes: &[],
                cwd: cwd.as_deref(),
                base: base.as_deref(),
                pgroup: self.group(),
                setsid: self.setsid,
                replace,
                long_entry_ends: !self.env.saw_path(),
                inherit_all: false,
                disclaim: self.disclaim,
                suspended: self.suspended,
            },
        )?;
        drop(handed);
        Ok((pid, ours))
    }
}

// ---------------------------------------------------------------------------
// Elsewhere: std's fork, with the strip in the child
// ---------------------------------------------------------------------------

/// A `std` child: on drop collected if it has exited, else handed to
/// [`crate::reaper`].
#[cfg(not(target_vendor = "apple"))]
#[derive(Debug)]
struct Handle {
    child: std::process::Child,
    status: Option<ExitStatus>,
}

#[cfg(not(target_vendor = "apple"))]
impl Handle {
    fn id(&self) -> u32 {
        self.child.id()
    }

    fn kill(&mut self) -> io::Result<()> {
        if self.status.is_some() {
            Ok(())
        } else {
            self.child.kill()
        }
    }

    fn resume(&mut self) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        let pid = i32::try_from(self.id()).map_err(io::Error::other)?;
        // SAFETY: two integers; the pid is our own unreaped child.
        if unsafe { super::kill(pid, super::SIGCONT) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let status = self.child.wait()?;
        self.status = Some(status);
        Ok(status)
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        self.status = self.child.try_wait()?;
        Ok(self.status)
    }
}

#[cfg(not(target_vendor = "apple"))]
impl Drop for Handle {
    fn drop(&mut self) {
        if matches!(self.try_wait(), Ok(None))
            && let Ok(pid) = i32::try_from(self.id())
        {
            crate::reaper::hand_off_pid(pid);
        }
    }
}

#[cfg(not(target_vendor = "apple"))]
impl Stdio {
    /// The same mode as `std`'s; a descriptor is duplicated, so the command
    /// keeps its own for a later spawn, as `std` keeps one.
    fn to_std(&self) -> io::Result<std::process::Stdio> {
        Ok(match &self.0 {
            Kind::Inherit => std::process::Stdio::inherit(),
            Kind::Null => std::process::Stdio::null(),
            Kind::Piped => std::process::Stdio::piped(),
            Kind::Fd(fd) => std::process::Stdio::from(fd.try_clone()?),
        })
    }
}

// The forked child's two calls, and `SIG_SETMASK` on Linux.
#[cfg(not(target_vendor = "apple"))]
unsafe extern "C" {
    fn sigprocmask(how: i32, set: *const u64, old: *mut u64) -> i32;
    #[link_name = "setsid"]
    fn new_session() -> i32;
}
#[cfg(not(target_vendor = "apple"))]
const SIG_SETMASK: i32 = 2;

#[cfg(not(target_vendor = "apple"))]
impl Command {
    /// Replace this process with the program: [`super::InheritStrip`] marks this process's
    /// own table first, so a failed exec leaves every descriptor close-on-exec
    /// ([`super::InheritNothing`] says what that means). The hooks run in this process, so a
    /// failed exec also leaves its signal mask empty and, with [`Self::setsid`], this process
    /// leading a new session (Darwin's `exec` changes nothing when it fails). Returns only on
    /// failure — at once, `Unsupported` and nothing run, beside
    /// [`Self::disclaim_responsibility`].
    pub fn exec(&mut self) -> io::Error {
        use std::os::unix::process::CommandExt as _;
        match self.forked() {
            Ok(mut command) => command.exec(),
            Err(error) => error,
        }
    }

    fn start(&mut self, fallback: Fallback, needs_stdin: bool) -> io::Result<Child> {
        let mut command = self.forked()?;
        if self.stdin.is_none() && !needs_stdin {
            command.stdin(std::process::Stdio::null());
        }
        if let Fallback::Piped = fallback {
            if self.stdout.is_none() {
                command.stdout(std::process::Stdio::piped());
            }
            if self.stderr.is_none() {
                command.stderr(std::process::Stdio::piped());
            }
        }
        let mut child = super::past_text_busy(
            super::TEXT_BUSY_FOR,
            || command.spawn(),
            super::text_busy_pause,
        )?;
        Ok(Child {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            handle: Handle {
                child,
                status: None,
            },
            usage: None,
        })
    }

    /// The `std` command this one is: [`super::InheritNothing`]'s strip
    /// first, then the empty mask and the session in one more hook. A
    /// disclaim is refused first of all — Darwin's responsibility SPI has no
    /// counterpart here, and a child asked to disclaim is never started
    /// without it — so `spawn`, `output`, `status` and `exec` all answer
    /// `Unsupported` with nothing started and this process untouched.
    fn forked(&self) -> io::Result<super::StdCommand> {
        use super::InheritNothing as _;
        use std::os::unix::process::CommandExt as _;
        if self.disclaim.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "disclaiming responsibility is Darwin's: no child is started without it",
            ));
        }
        // A fork can stop its child before the exec, never after it: nothing here gives what
        // the flag promises, so nothing is started.
        if self.suspended {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "starting a child stopped at its exec is Darwin's: no child is started",
            ));
        }
        #[expect(
            clippy::disallowed_types,
            reason = "spawnfd's own Command, started through std's fork with the strip installed first"
        )]
        let mut command = std::process::Command::new(&self.program);
        command.inherit_nothing().args(&self.args);
        if let Some(arg0) = &self.arg0 {
            command.arg0(arg0);
        }
        if self.env.clear {
            command.env_clear();
        }
        for (key, value) in &self.env.vars {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        if let Some(dir) = &self.cwd {
            command.current_dir(dir);
        }
        if let Some(stdin) = &self.stdin {
            command.stdin(stdin.to_std()?);
        }
        if let Some(stdout) = &self.stdout {
            command.stdout(stdout.to_std()?);
        }
        if let Some(stderr) = &self.stderr {
            command.stderr(stderr.to_std()?);
        }
        if let Some(pgroup) = self.group() {
            command.process_group(pgroup);
        }
        let setsid = self.setsid;
        // SAFETY: the hook runs between fork and exec and calls only
        // `sigprocmask` and `setsid`, async-signal-safe, on a stack array.
        unsafe {
            command.pre_exec(move || {
                let empty = [0u64; 16];
                sigprocmask(SIG_SETMASK, empty.as_ptr(), std::ptr::null_mut());
                if setsid && new_session() < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(command)
    }
}
