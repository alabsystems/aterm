// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Non-Unix `spawnfd` surface providing [`Command`], [`command`], [`transparent`], and [`InheritNothing`].
//!
//! On Windows, `CreateProcessW` on a file that is no image for this machine (a script, a
//! truncated download) raises the system box "This version of %1 is not compatible" and waits in
//! the starting thread until a person answers it, unless that thread's or its process's error
//! mode turns the box off; std holds its process-wide spawn lock across the call, so every other
//! start of this process waits behind it. [`Command`]'s starts ([`Command::spawn`],
//! [`Command::output`], [`Command::status`]) therefore run with the starting thread's boxes off
//! (`aterm_sysprobe::quiet_errors`) and the bad image comes back as the start's error
//! (`ERROR_BAD_EXE_FORMAT`, 193, or `ERROR_EXE_MACHINE_TYPE_MISMATCH`, 216). The guard is the
//! thread's mode (`SetThreadErrorMode`), restored when [`Command::spawn`] returns, and when
//! [`Command::output`] or [`Command::status`] returns, after the child's end: their wait and pipe
//! reads run on that thread, which starts nothing else meanwhile. A child inherits its parent's
//! process mode (`SetErrorMode`, `GetErrorMode`), never a thread's, so the child keeps its normal
//! error mode. [`Command`] wraps std's builder and hands out no `&mut` of it, so no
//! caller can start a seam-built command past the guard.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::{ExitStatus, Output};

/// Alias for `std::process::Command` at reviewed raw spawn sites.
#[expect(
    clippy::disallowed_types,
    reason = "the one name a reviewed raw std Command is taken by; off Unix spawnfd's Command wraps std's"
)]
pub type StdCommand = std::process::Command;

pub use crate::{SOFT_NOFILE_FLOOR, raised_soft_nofile_target};
pub use std::process::{Child, Stdio};

/// The child builder every non-Unix spawn site takes ([`command`]): std's, started with the
/// starting thread's system error boxes off (module docs).
pub struct Command(StdCommand);

impl Command {
    /// `program` with no arguments, this process's environment and working directory, and
    /// stdio left for the start to decide (std's defaults). [`command`] adds the console rule.
    #[must_use]
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        #[expect(
            clippy::disallowed_types,
            reason = "the seam's own builder: the one std Command it wraps"
        )]
        let inner = std::process::Command::new(program);
        Self(inner)
    }

    /// Append one argument.
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.0.arg(arg);
        self
    }

    /// Append arguments.
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.0.args(args);
        self
    }

    /// Append `raw` to the command line verbatim, unquoted (std's `CommandExt::raw_arg`).
    #[cfg(windows)]
    pub fn raw_arg(&mut self, raw: impl AsRef<OsStr>) -> &mut Self {
        use std::os::windows::process::CommandExt as _;
        self.0.raw_arg(raw);
        self
    }

    /// Set one environment variable for the child.
    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.0.env(key, value);
        self
    }

    /// Set environment variables for the child.
    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.0.envs(vars);
        self
    }

    /// Remove one environment variable from the child's.
    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.0.env_remove(key);
        self
    }

    /// Start the child with an empty environment (before any [`Self::env`]).
    pub fn env_clear(&mut self) -> &mut Self {
        self.0.env_clear();
        self
    }

    /// The child's working directory.
    pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
        self.0.current_dir(dir);
        self
    }

    /// The child's stdin.
    pub fn stdin(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.0.stdin(cfg);
        self
    }

    /// The child's stdout.
    pub fn stdout(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.0.stdout(cfg);
        self
    }

    /// The child's stderr.
    pub fn stderr(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.0.stderr(cfg);
        self
    }

    /// The program the child runs.
    #[must_use]
    pub fn get_program(&self) -> &OsStr {
        self.0.get_program()
    }

    /// The child's arguments, the program excluded.
    pub fn get_args(&self) -> impl ExactSizeIterator<Item = &OsStr> {
        self.0.get_args()
    }

    /// The environment changes made for the child (`None`: removed).
    pub fn get_envs(&self) -> impl ExactSizeIterator<Item = (&OsStr, Option<&OsStr>)> {
        self.0.get_envs()
    }

    /// The child's working directory, when set.
    #[must_use]
    pub fn get_current_dir(&self) -> Option<&Path> {
        self.0.get_current_dir()
    }

    /// Start the child, with this thread's system error boxes off for the start (module docs).
    ///
    /// # Errors
    /// What std's start answered: a file that is no image for this machine is
    /// `ERROR_BAD_EXE_FORMAT` (193) or `ERROR_EXE_MACHINE_TYPE_MISMATCH` (216), never a box.
    pub fn spawn(&mut self) -> io::Result<Child> {
        let _quiet = quiet();
        self.0.spawn()
    }

    /// Run the child to its end with stdin null and stdout and stderr captured (unless set
    /// otherwise), its start under [`Self::spawn`]'s rule; this thread's boxes stay off until
    /// it returns (module docs).
    ///
    /// # Errors
    /// As [`Self::spawn`]'s, and reading or waiting.
    pub fn output(&mut self) -> io::Result<Output> {
        let _quiet = quiet();
        self.0.output()
    }

    /// Run the child to its end with unset stdio inherited, its start under [`Self::spawn`]'s
    /// rule; this thread's boxes stay off until it returns (module docs).
    ///
    /// # Errors
    /// As [`Self::spawn`]'s, and waiting.
    pub fn status(&mut self) -> io::Result<ExitStatus> {
        let _quiet = quiet();
        self.0.status()
    }

    /// The wrapped builder, for this crate's console rule ([`crate::spawn`]): never handed out,
    /// so every start stays [`Self::spawn`]'s.
    pub(crate) fn std_mut(&mut self) -> &mut StdCommand {
        &mut self.0
    }
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// This thread's system error boxes off while the answer lives (module docs); nothing off Windows.
fn quiet() -> impl Sized {
    #[cfg(windows)]
    {
        aterm_sysprobe::quiet_errors()
    }
}

/// The Unix soft descriptor raise, a no-op off Unix: no `RLIMIT_NOFILE`
/// governs a Windows process's handles.
pub fn raise_soft_nofile() {}

/// Run `spawn` once (non-Unix twin of the Unix `ETXTBSY` retry helper).
///
/// # Errors
/// What `spawn` answered.
pub fn spawn_past_text_busy<T>(mut spawn: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    spawn()
}

/// The Unix descriptor-strip trait, a no-op off Unix.
pub trait InheritNothing {
    /// No-op off Unix.
    fn inherit_nothing(&mut self) -> &mut Self;
}

impl InheritNothing for StdCommand {
    fn inherit_nothing(&mut self) -> &mut Self {
        self
    }
}

impl InheritNothing for Command {
    fn inherit_nothing(&mut self) -> &mut Self {
        self
    }
}

/// The Unix native-image spawn, a no-op off Unix: Windows starts no shell over a file it will
/// not execute, so the spawn already fails with the loader's error.
pub trait NativeImage {
    /// No-op off Unix.
    fn native_image(&mut self, native: bool) -> &mut Self;
}

impl NativeImage for Command {
    fn native_image(&mut self, _native: bool) -> &mut Self {
        self
    }
}

/// The Unix death tie, a no-op off Unix.
pub trait Tether {
    /// No-op off Unix.
    fn tether(&mut self) -> &mut Self;
}

impl Tether for Command {
    fn tether(&mut self) -> &mut Self {
        self
    }
}

/// Build `Command::new(program)` configured with quiet Windows console flags.
#[must_use]
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    crate::spawn::set_flags(&mut command, 0);
    command
}

/// Child handle started by [`transparent`].
pub type TransparentChild = Child;

/// Pass-through child builder off Unix, delegating to [`command`].
#[must_use]
pub fn transparent(program: impl AsRef<OsStr>) -> Command {
    command(program)
}

#[cfg(all(test, windows))]
#[path = "spawnfd_nonunix_tests.rs"]
mod tests;
