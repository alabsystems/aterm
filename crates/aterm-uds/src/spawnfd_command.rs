// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`Command`], [`Stdio`] and [`Child`]: explicit stdio and guard inheritance, started on
//! Darwin by one `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`, with no parent fork.

use super::{DISCLAIM_SPI, F_GETFL, F_SETFL, fcntl};
use std::collections::BTreeMap;
use std::ffi::{CStr, OsStr, OsString};
use std::fmt;
use std::io::{self, Read as _};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdin, ChildStdout, ExitStatus, Output};
use std::sync::Arc;

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
    pub const fn piped() -> Self {
        Self(Kind::Piped)
    }

    /// This process's own descriptor at that number.
    #[must_use]
    pub const fn inherit() -> Self {
        Self(Kind::Inherit)
    }

    /// `/dev/null`.
    #[must_use]
    pub const fn null() -> Self {
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
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
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

struct Guard {
    source: OwnedFd,
    target: RawFd,
}

/// A native command whose default child holds stdio and explicitly named guards. A converted
/// [`super::TransparentCommand`] retains the caller's non-close-on-exec descriptors instead.
pub struct Command {
    program: OsString,
    arg0: Option<OsString>,
    args: Vec<OsString>,
    env: Env,
    cwd: Option<PathBuf>,
    stdin: Option<Stdio>,
    stdout: Option<Stdio>,
    stderr: Option<Stdio>,
    #[cfg(unix)]
    guards: Vec<Guard>,
    /// Pass-through commands retain the caller's non-close-on-exec table. Ordinary commands
    /// still hand on only explicitly named guards.
    inherit_all: bool,
    /// Start with default dispositions for every catchable signal, including inherited ignores.
    default_signals: bool,
    /// An in-place native exec keeps this thread's signal mask instead of starting empty.
    #[cfg(target_vendor = "apple")]
    inherit_exec_signal_mask: bool,
    pgroup: Option<i32>,
    setsid: bool,
    /// The SPI that makes the child its own responsible process, when asked
    /// ([`Self::disclaim_responsibility`]): always [`DISCLAIM_SPI`] but in a
    /// test that names one no image exports.
    disclaim: Option<&'static CStr>,
    /// Start the child stopped at its exec ([`Self::start_suspended`]).
    suspended: bool,
    /// Run the program's own image, never a shell over it ([`super::NativeImage`]).
    native_image: bool,
    /// The task type the child starts as ([`Self::leave_background_band`]).
    task: Option<super::TaskType>,
    #[cfg(target_os = "macos")]
    footprint: Option<trust_exec_policy::FootprintLimit>,
    /// End the child when this process dies ([`super::Tether`]).
    tether: bool,
    /// Fork a tethered child on the calling thread ([`Self::tie_to_this_thread`]).
    #[cfg(all(test, target_os = "linux"))]
    this_thread: bool,
}

/// The stdio a spawn gives a slot the caller left unset.
#[derive(Clone, Copy)]
enum Fallback {
    Inherit,
    Piped,
}

impl Command {
    fn with_dedicated_process_group<T>(&mut self, operation: impl FnOnce(&mut Self) -> T) -> T {
        self.with_temporary_process_group(0, operation)
    }

    fn with_temporary_process_group<T>(
        &mut self,
        group: i32,
        operation: impl FnOnce(&mut Self) -> T,
    ) -> T {
        struct Restore<'a> {
            command: &'a mut Command,
            pgroup: Option<i32>,
        }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                self.command.pgroup = self.pgroup;
            }
        }

        let configured = self.pgroup.replace(group);
        let restore = Restore {
            command: self,
            pgroup: configured,
        };
        operation(&mut *restore.command)
    }

    /// `program` with no arguments, this process's environment and working directory, and stdio inherited (`spawn`/`status`) or piped (`output`).
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
            #[cfg(unix)]
            guards: Vec::new(),
            inherit_all: false,
            default_signals: false,
            #[cfg(target_vendor = "apple")]
            inherit_exec_signal_mask: false,
            pgroup: None,
            setsid: false,
            disclaim: None,
            suspended: false,
            native_image: false,
            task: None,
            #[cfg(target_os = "macos")]
            footprint: None,
            tether: false,
            #[cfg(all(test, target_os = "linux"))]
            this_thread: false,
        }
    }

    pub(super) fn inheriting(program: impl AsRef<OsStr>) -> Self {
        let mut command = Self::new(program);
        command.inherit_all = true;
        command
    }

    /// The child's `argv[0]`, in place of the program as it was named (which is still what is searched for and run) — `std::os::unix::process::CommandExt::arg0`'s.
    pub fn arg0(&mut self, arg0: impl AsRef<OsStr>) -> &mut Self {
        self.arg0 = Some(arg0.as_ref().to_owned());
        self
    }
    /// The explicit lossless argv[0] override, if one was configured.
    pub fn get_arg0(&self) -> Option<&OsStr> {
        self.arg0.as_deref()
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

    /// Replace all arguments, preserving the program and launch configuration.
    pub fn set_args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args.clear();
        self.args(args)
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

    /// Start the child's environment empty: only what `env`/`envs` set after this reaches it.
    pub fn env_clear(&mut self) -> &mut Self {
        self.env.clear();
        self
    }

    /// The child's working directory.
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

    /// Reset every catchable signal to `SIG_DFL` in the child before its image starts.
    ///
    /// Ordinary commands preserve inherited ignored dispositions (apart from SIGPIPE, as std
    /// does). A check runner opts in so a background shell's ignored SIGINT/SIGQUIT cannot
    /// change a test's behavior. This never changes the parent's dispositions or the command's
    /// signal-mask policy. The new image may install its own handlers after exec.
    pub const fn default_signals(&mut self) -> &mut Self {
        self.default_signals = true;
        self
    }

    /// Start the child in process group `pgroup` (`0`: a new group of its own, led by the child) — `std::os::unix::process::CommandExt`'s.
    pub const fn process_group(&mut self, pgroup: i32) -> &mut Self {
        self.pgroup = Some(pgroup);
        self
    }

    /// Inherit only these explicitly named guard descriptions at [`Self::guard_targets`].
    /// Each descriptor retains its already occupied number, so inheritance cannot clobber
    /// stdio, a bridge lane or a native spawn's private status pipe.
    /// The child receives non-close-on-exec slots; they close when it closes them or exits.
    ///
    /// # Errors
    /// Too many guards, an unplaceable descriptor, or failure to keep its parent description
    /// out of other children.
    #[cfg(unix)]
    pub fn inherit_guards(&mut self, guards: Vec<std::os::fd::OwnedFd>) -> io::Result<&mut Self> {
        if guards.len() > super::MAX_GUARD_FDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many explicit spawn guards",
            ));
        }
        let mut placed = Vec::with_capacity(guards.len());
        for guard in guards {
            let guard = super::off_the_targets(guard)?;
            crate::keep_out_of_children(&guard)?;
            placed.push(Guard {
                target: guard.as_raw_fd(),
                source: guard,
            });
        }
        self.guards = placed;
        Ok(self)
    }

    /// Preserve exact already-owned descriptor numbers across an in-place
    /// native exec, including slots3/4. Ordinary guard inheritance deliberately
    /// relocates those reserved bridge slots. No mapping or occupied-number
    /// replacement is performed here; every source remains its own target.
    /// A native exec privately copies its sources before applying file actions.
    /// This supplies no resource entitlement or authority to adopt raw slots.
    #[cfg(unix)]
    pub fn inherit_exec_guards(&mut self, guards: Vec<OwnedFd>) -> io::Result<&mut Self> {
        #[cfg(not(target_vendor = "apple"))]
        {
            let _ = guards;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "exact exec guards require the native Apple launch boundary",
            ))
        }
        #[cfg(target_vendor = "apple")]
        {
            let mut targets = std::collections::BTreeSet::new();
            if guards.len() > super::MAX_GUARD_FDS
                || guards.iter().any(|guard| {
                    let fd = guard.as_raw_fd();
                    !(3..super::ACTION_FD_LIMIT).contains(&fd) || !targets.insert(fd)
                })
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid original exec guard slots",
                ));
            }
            let mut placed = Vec::with_capacity(guards.len());
            for source in guards {
                crate::keep_out_of_children(&source)?;
                placed.push(Guard {
                    target: source.as_raw_fd(),
                    source,
                });
            }
            self.guards = placed;
            Ok(self)
        }
    }

    /// Preserve the calling thread's current signal mask across this command's in-place native
    /// exec. Spawned children still start with no signal blocked, and unsupported platforms
    /// refuse before changing command state.
    ///
    /// The caller must arrange the receiver's handler installation and selective mask restore;
    /// carrying a mask grants neither a signal disposition nor permission to unblock it.
    ///
    /// # Errors
    /// This platform has no qualified native in-place exec boundary.
    #[cfg(unix)]
    pub fn inherit_exec_signal_mask(&mut self) -> io::Result<&mut Self> {
        #[cfg(not(target_vendor = "apple"))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "signal-mask handoff requires the native Apple exec boundary",
            ))
        }
        #[cfg(target_vendor = "apple")]
        {
            self.inherit_exec_signal_mask = true;
            Ok(self)
        }
    }

    /// Inherit these owned descriptions at explicit child-only descriptor numbers.
    /// Private source copies are kept off every target before native file actions;
    /// no occupied descriptor in this process is replaced or unmarked.
    ///
    /// # Errors
    /// More than 130 guards, duplicate targets, stdio/out-of-bound targets, unavailable
    /// descriptor copies, or a platform without native mapped placement. Relocated
    /// guards cannot be used with in-place exec, whose failure preserves this table.
    #[cfg(unix)]
    pub fn inherit_guards_at(&mut self, guards: Vec<(OwnedFd, RawFd)>) -> io::Result<&mut Self> {
        #[cfg(not(target_vendor = "apple"))]
        {
            let _ = guards;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "mapped guards need native spawn actions",
            ))
        }
        #[cfg(target_vendor = "apple")]
        {
            let mut limit = super::Rlimit { cur: 0, max: 0 };
            // SAFETY: getrlimit writes exactly the live platform Rlimit storage.
            if unsafe { super::getrlimit(super::RLIMIT_NOFILE, &mut limit) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let bound = super::ACTION_FD_LIMIT.min(super::descriptor_bound()) as u64;
            let bound = bound.min(limit.cur);
            let mut targets = std::collections::BTreeSet::new();
            if guards.len() > super::MAX_GUARD_FDS
                || guards.iter().any(|(_, at)| {
                    *at < 3
                        || u64::try_from(*at).ok().is_none_or(|at| at >= bound)
                        || !targets.insert(*at)
                })
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid explicit guard targets",
                ));
            }
            let mut placed = Vec::with_capacity(guards.len());
            let mut excluded = Vec::new();
            for (source, target) in guards {
                let source = loop {
                    let copy = super::placeable_copy(source.as_raw_fd())?;
                    if !targets.contains(&copy.as_raw_fd()) {
                        break copy;
                    }
                    // Keep excluded fresh slots occupied until all copies are
                    // placed; repeated dup calls cannot choose them again.
                    excluded.push(copy);
                };
                placed.push(Guard { source, target });
            }
            self.guards = placed;
            Ok(self)
        }
    }

    /// Add mapped guards without replacing already selected resource custody.
    /// All descriptions are prepared together so a new target cannot overwrite
    /// an earlier source during child placement. Failure preserves this command's
    /// original table; the supplied additions are consumed without being installed.
    #[cfg(unix)]
    pub fn append_guards_at(&mut self, guards: Vec<(OwnedFd, RawFd)>) -> io::Result<&mut Self> {
        let count = self
            .guards
            .len()
            .checked_add(guards.len())
            .filter(|count| *count <= super::MAX_GUARD_FDS)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "too many explicit spawn guards",
                )
            })?;
        let mut combined = Vec::with_capacity(count);
        for guard in &self.guards {
            // Preserve the native fork exclusion while assembling the new table,
            // before inherit_guards_at makes its final placeable copies.
            combined.push((crate::kept_out(|| guard.source.try_clone())?, guard.target));
        }
        combined.extend(guards);
        self.inherit_guards_at(combined)
    }

    /// Add close-only copies of another command's original guards at the same
    /// explicit targets. This neither consumes nor changes the source table.
    /// # Errors
    /// Copying a descriptor or preparing the combined table failed; both
    /// original command tables are preserved.
    #[cfg(unix)]
    pub fn append_guards_from(&mut self, source: &Self) -> io::Result<&mut Self> {
        let guards = source
            .guards
            .iter()
            .map(|guard| crate::kept_out(|| guard.source.try_clone()).map(|fd| (fd, guard.target)))
            .collect::<io::Result<Vec<_>>>()?;
        self.append_guards_at(guards)
    }

    /// Stable child descriptor numbers after either inheritance method, available before spawn
    /// so an authenticated launch can journal each actual guard's identity and position.
    #[cfg(unix)]
    pub fn guard_targets(&self) -> impl ExactSizeIterator<Item = RawFd> + '_ {
        self.guards.iter().map(|guard| guard.target)
    }

    /// Start the child in a new session of its own (`setsid`), with no controlling terminal.
    pub const fn setsid(&mut self, setsid: bool) -> &mut Self {
        self.setsid = setsid;
        self
    }

    /// Start the child as its OWN RESPONSIBLE PROCESS (`true`), not charged to the one this process is charged to. macOS attributes a process's TCC and syspolicyd questions — a privacy grant, the Developer Tools verdict — to its responsible process, which a child inherits from a parent that has one (a shell's child under an app is asked about as that app).
    pub fn disclaim_responsibility(&mut self, disclaim: bool) -> &mut Self {
        self.disclaim = disclaim.then_some(DISCLAIM_SPI);
        self
    }

    /// Start the child STOPPED AT ITS EXEC (`true`): `spawn` returns once the kernel has exec'd the program — its image mapped and its code signature checked — and not one instruction of the program, nor of the dynamic loader before it, runs until [`Child::resume`].
    pub const fn start_suspended(&mut self, suspended: bool) -> &mut Self {
        self.suspended = suspended;
        self
    }

    /// Whether the command requests a stopped-at-exec start; observing policy is
    /// not evidence that a child exists or has acquired foreground custody.
    #[must_use]
    pub const fn starts_suspended(&self) -> bool {
        self.suspended
    }

    /// Prepare equal active/inactive fatal physical-footprint limits for this
    /// native spawn or SETEXEC. This is not an RSS reservation or a receipt that
    /// a child ran with the policy. A later exec needs its own policy; launch
    /// owners must identify the actual final image and retain original custody.
    #[cfg(target_os = "macos")]
    pub const fn footprint_limit(&mut self, limit: trust_exec_policy::FootprintLimit) -> &mut Self {
        self.footprint = Some(limit);
        self
    }

    /// Requested final-exec attributes, not an installed-policy observation.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub const fn get_footprint_limit(&self) -> Option<trust_exec_policy::FootprintLimit> {
        self.footprint
    }

    /// Remove the requested attributes for transfer to a final selected exec.
    /// This moves launch configuration only, not an installed policy or a
    /// memory reservation. After extraction this command carries no such cap.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn take_footprint_limit(&mut self) -> Option<trust_exec_policy::FootprintLimit> {
        self.footprint.take()
    }

    /// Start the child, or replace this process, OUT OF THIS PROCESS'S BACKGROUND BAND (`true`):
    /// on macOS as a standard
    /// daemon task (`POSIX_SPAWN_PROC_TYPE_DAEMON_STANDARD`) in place of the task type it would
    /// inherit, so a child of a launchd job of `ProcessType` `Background`, whose every thread runs
    /// at priority 4 whatever class it asks for (changing priority there cannot leave the band),
    /// runs each of its threads at the class that thread asks for. Measured on macOS 27.0 in a
    /// private-label `Background` job: a plain child's thread read 4 at the default class, one
    /// started so read 31 there and 20 at utility. A QoS clamp this process runs under (a
    /// `Standard` job's, `taskpolicy -c`) and a darwin-bg set on it from outside (`taskpolicy
    /// -b`) are the child's still. Elsewhere there is no such band, and nothing changes.
    pub const fn leave_background_band(&mut self, leave: bool) -> &mut Self {
        self.task = if leave {
            Some(super::TaskType::Standard)
        } else {
            None
        };
        self
    }

    /// Whether the child starts out of this process's background band
    /// ([`Self::leave_background_band`]).
    #[must_use]
    pub const fn leaves_background_band(&self) -> bool {
        matches!(self.task, Some(super::TaskType::Standard))
    }

    /// Start the child IN macOS's background band: as a background daemon task
    /// (`POSIX_SPAWN_PROC_TYPE_DAEMON_BACKGROUND`), the type launchd starts a job of
    /// `ProcessType` `Background` as, every thread of it at priority 4 (darwin-bg) whatever class
    /// it asks for. A process that measures that job's band starts its stand-in so. Elsewhere
    /// there is no such band, and nothing changes.
    pub const fn enter_background_band(&mut self) -> &mut Self {
        self.task = Some(super::TaskType::Background);
        self
    }

    /// Start the child AS AN INTERACTIVE DAEMON: as the task type
    /// (`POSIX_SPAWN_PROC_TYPE_DAEMON_INTERACTIVE`) launchd starts a job of `ProcessType`
    /// `Interactive` as, in place of the type it would inherit, so a child of a process in the
    /// background band leaves the band and runs each thread at the class it asks for. A thread's
    /// priority does not tell it from a standard daemon ([`Self::leave_background_band`]), and it
    /// promises no more: a standard daemon was measured to keep a QoS clamp this process runs
    /// under (`taskpolicy -c`) and a darwin-bg set on it from outside (`taskpolicy -b`).
    /// Elsewhere there is no task type, and nothing changes.
    pub const fn start_as_interactive_daemon(&mut self) -> &mut Self {
        self.task = Some(super::TaskType::Interactive);
        self
    }

    /// TEST SEAM: disclaim through the SPI named `spi` instead of [`DISCLAIM_SPI`] — one no image exports, to drive the fail-closed path.
    #[cfg(test)]
    pub(super) fn disclaim_through(&mut self, spi: &'static CStr) -> &mut Self {
        self.disclaim = Some(spi);
        self
    }

    /// TEST SEAM: fork a tethered child on the calling thread, not on one of the tether's
    /// spawners, so Linux's parent-death signal follows that thread and the child dies when the
    /// thread exits: the per-thread tie the spawners replace, kept as their control.
    #[cfg(all(test, target_os = "linux"))]
    pub(super) const fn tie_to_this_thread(&mut self) -> &mut Self {
        self.this_thread = true;
        self
    }

    /// The group the spawn asks for: none beside `setsid` when the group
    /// asked is `0`, which the session gives anyway.
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

    /// Whether the child starts as its own responsible process
    /// ([`Self::disclaim_responsibility`]).
    #[must_use]
    pub const fn get_disclaim_responsibility(&self) -> bool {
        self.disclaim.is_some()
    }

    /// Start the child; unset stdio is inherited.
    /// # Errors
    /// As `std`'s: `InvalidInput` for a NUL, `NotFound` for a program (or a working directory) that does not exist, `PermissionDenied` for one that may not be run, and whatever else the spawn answers.
    pub fn spawn(&mut self) -> io::Result<Child> {
        self.start(Fallback::Inherit, true)
    }

    /// Consume one actual attempt and observe this command's actual spawn outcome without
    /// changing its configured process group or session policy. Persist the key before calling
    /// when this launch carries a durable obligation. The result supplies an owned child,
    /// not a dedicated-group birth proof.
    ///
    /// # Errors
    /// This attempt's process or scope changed, or the same controlled spawn errors as
    /// [`Self::spawn`]. Only a current pre-birth failure supplies a no-child receipt. A postbirth error retains the actual receipt.
    pub fn spawn_observed(
        &mut self,
        attempt: super::BirthAttempt,
    ) -> Result<super::ObservedSpawned, super::SpawnFailure<super::ObservedSpawned>> {
        super::birth::observe(attempt, || self.start_pending(Fallback::Inherit, true))
    }

    /// Consume one actual attempt and start a child in a new, self-led process group. Persist
    /// the attempt's key before calling; bind the returned actual child before acknowledging
    /// its work. An error is an opaque receipt for this attempt alone, not a reusable flag.
    /// The dedicated group holds for this one spawn only: the command's configured process
    /// group is restored when this call returns or unwinds, so a consumed attempt — spawned,
    /// refused or failed alike — never reconfigures the command's later births.
    ///
    /// # Errors
    /// This attempt's process or scope changed, or the same controlled spawn errors as
    /// [`Self::spawn`]. Postbirth observation failure retains the actual child and does not certify no target execution.
    pub fn spawn_cohort(
        &mut self,
        attempt: super::BirthAttempt,
    ) -> Result<super::Spawned, super::SpawnFailure> {
        self.with_dedicated_process_group(|command| {
            super::birth::observe_cohort(attempt, || command.start_pending(Fallback::Inherit, true))
        })
    }

    /// Start the child in a new, self-led process group for this one spawn only: the
    /// command's configured process group is restored when this call returns or unwinds,
    /// so a borrowed command's later births keep the group their caller configured.
    /// Unlike [`Self::spawn_cohort`], no birth attempt is consumed and no receipt is
    /// minted: this is for an owner that holds the child itself, not a durable launch.
    /// # Errors
    /// As [`Self::spawn`]'s.
    pub fn spawn_in_dedicated_process_group(&mut self) -> io::Result<Child> {
        self.with_dedicated_process_group(|command| command.spawn())
    }

    /// Start the child as a member of the existing process group `group` for this one
    /// spawn only: the command's configured process group is restored when this call
    /// returns or unwinds, so a borrowed command's later births keep the group their
    /// caller configured. No birth attempt is consumed and no receipt is minted: the
    /// result is an ordinary owned [`Child`], as [`Self::spawn`]'s. The integer is not
    /// a capability: the caller owns keeping that group alive and proving it is the
    /// group it meant, and the kernel still refuses a group the child may not join.
    /// # Errors
    /// `InvalidInput` before anything is dispatched when `group` is at most 1, when a
    /// new session was asked for ([`Self::setsid`]), or when the child is tethered
    /// ([`super::Tether`]): a session or a tethered birth would not land the target in
    /// the group asked for, and neither setting is reset to make it fit. Otherwise as
    /// [`Self::spawn`]'s, a kernel refusal to join the group included.
    pub fn spawn_in_process_group(&mut self, group: i32) -> io::Result<Child> {
        if group <= 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a process group to join is greater than 1",
            ));
        }
        if self.setsid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a child starting in a new session cannot join an existing process group",
            ));
        }
        if self.tether {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a tethered child cannot join an existing process group",
            ));
        }
        self.with_temporary_process_group(group, |command| command.spawn())
    }

    /// Run the child to its end with stdin `/dev/null` and stdout and stderr captured (unless set otherwise).
    /// # Errors
    /// As [`Self::spawn`]'s, and reading or waiting.
    pub fn output(&mut self) -> io::Result<Output> {
        self.start(Fallback::Piped, false)?.wait_with_output()
    }

    /// Run the child to its end with unset stdio inherited.
    /// # Errors
    /// As [`Self::spawn`]'s, and waiting.
    pub fn status(&mut self) -> io::Result<ExitStatus> {
        self.start(Fallback::Inherit, true)?.wait()
    }
}

#[cfg(test)]
#[path = "spawnfd_command_own_tests.rs"]
mod tests;

impl super::Tether for Command {
    fn tether(&mut self) -> &mut Self {
        self.tether = true;
        self
    }
}

impl super::NativeImage for Command {
    fn native_image(&mut self, native: bool) -> &mut Self {
        self.native_image = native;
        self
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

/// A child [`Command`] started: `std::process::Child`'s shape, with `std`'s own pipe types in the three public fields.
pub struct Child {
    /// The parent's end of a piped stdin.
    pub stdin: Option<ChildStdin>,
    /// The parent's end of a piped stdout.
    pub stdout: Option<ChildStdout>,
    /// The parent's end of a piped stderr.
    pub stderr: Option<ChildStderr>,
    foreground: Option<foreground::Lease>,
    handle: Handle,
    usage: Option<Rusage>,
    custody: Arc<()>,
}

/// A child's exit status and the resources it and the descendants it reaped used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rusage {
    pub status: ExitStatus,
    /// CPU time spent in user mode, as reported by the owning kernel wait.
    pub user: std::time::Duration,
    /// CPU time spent in kernel mode, as reported by the same owning wait.
    pub system: std::time::Duration,
    /// User plus system CPU time.
    pub cpu: std::time::Duration,
    /// The largest process's resident peak, in bytes on every host.
    pub maxrss: u64,
}

#[path = "spawnfd_rusage.rs"]
mod rusage;

#[path = "spawnfd_foreground.rs"]
mod foreground;

#[path = "spawnfd_job.rs"]
mod job;
pub use job::{ChildEvent, ChildNotice, ChildObservation, ChildStop};

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
    /// Parent-private handle identity, stable across moves. A retained receipt
    /// prevents allocation reuse; replacing a Child cannot replace this binding.
    pub(super) fn custody_key(&self) -> Arc<()> {
        Arc::clone(&self.custody)
    }

    pub(super) fn owns_custody(&self, key: &Arc<()>) -> bool {
        self.handle.require_custody().is_ok() && Arc::ptr_eq(&self.custody, key)
    }

    fn observe_wait_result<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        if let Err(error) = &result {
            self.handle.observe_wait_error(error);
            if self.handle.require_custody().is_err()
                && let Some(lease) = self.foreground.as_mut()
            {
                lease.disarm();
            }
        }
        result
    }

    /// Whether this actual handle still pins an unreaped child's numeric identity.
    #[must_use]
    pub fn owns_unreaped(&self) -> bool {
        !self.is_reaped() && self.handle.require_custody().is_ok()
    }

    /// Wait for this actual child's terminal exit without reaping it. Stop,
    /// trace and continue events do not establish completion. The cached result
    /// of a prior reap is returned without supplying a new identity pin.
    /// # Errors
    /// Exit observation or the kernel exit watch fails. `ECHILD` permanently forfeits
    /// numeric custody; other observation failures leave ownership unchanged.
    pub fn wait_exited(&mut self) -> io::Result<()> {
        self.handle.require_custody()?;
        if self.usage.is_some() || self.handle.status.is_some() {
            return Ok(());
        }
        let result = rusage::observe_exit(self.id(), true);
        if self.observe_wait_result(result)? {
            Ok(())
        } else {
            Err(io::Error::other(
                "no terminal exit observed for owned child",
            ))
        }
    }

    /// Give this actual live self-led child's group the caller's controlling terminal.
    /// The caller must currently own its foreground. Start suspended, hand over, then
    /// [`Self::resume`] to avoid a background terminal-read race. Every owning wait
    /// restores foreground before reaping; Drop attempts restoration without reaping.
    /// An intervening foreground handoff is respected. This is cooperative job control,
    /// not an atomic exclusion of other terminal controllers.
    ///
    /// # Errors
    /// Reaped/exited child, non-self-led or foreign-session group, another active lease,
    /// unavailable terminal, background caller, or terminal/signal-mask operation failure.
    /// A transfer failure retains its lease here until restoration can be retried.
    pub fn take_foreground(&mut self, terminal: std::fs::File) -> io::Result<()> {
        if self.foreground.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "child already holds terminal foreground custody",
            ));
        }
        if self.exited()? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal handoff requires a live unreaped child",
            ));
        }
        self.foreground = Some(foreground::Lease::prepare(self.id(), terminal)?);
        self.foreground.as_mut().expect("prepared lease").take()
    }

    /// Return terminal foreground while this child is still owned and unreaped.
    /// A terminal another job already owns is left alone. Retain failed restoration
    /// for retry; no wait may reap this child until restoration succeeds.
    /// # Errors
    /// Its controlling process/session changed, or a terminal/mask operation failed.
    pub fn restore_foreground(&mut self) -> io::Result<()> {
        self.handle.require_custody()?;
        if self.foreground.is_some() && (self.usage.is_some() || self.handle.status.is_some()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal custody must be restored before reaping",
            ));
        }
        if let Some(lease) = self.foreground.as_mut() {
            lease.restore()?;
            self.foreground = None;
        }
        Ok(())
    }

    fn before_reap(&mut self, block: bool) -> io::Result<bool> {
        self.handle.require_custody()?;
        if self.foreground.is_none() {
            return Ok(true);
        }
        let result = rusage::observe_exit(self.id(), block);
        if !self.observe_wait_result(result)? {
            if block {
                return Err(io::Error::other(
                    "blocking wait observed no owned-child exit",
                ));
            }
            return Ok(false);
        }
        self.restore_foreground()?;
        Ok(true)
    }

    /// Wait for the child and collect its resource accounting.
    /// # Errors
    /// `wait4`'s, or `Unsupported` off 64-bit Darwin/Linux.
    pub fn wait_with_rusage(&mut self) -> io::Result<Rusage> {
        drop(self.stdin.take());
        self.collect_rusage(true)?
            .ok_or_else(|| io::Error::other("wait4 returned no exit status"))
    }

    /// Collect status and resource accounting if the child has exited, else `None`.
    /// # Errors
    /// As [`Self::wait_with_rusage`].
    pub fn try_wait_with_rusage(&mut self) -> io::Result<Option<Rusage>> {
        self.collect_rusage(false)
    }

    fn collect_rusage(&mut self, block: bool) -> io::Result<Option<Rusage>> {
        self.handle.require_custody()?;
        if let Some(usage) = self.usage {
            return Ok(Some(usage));
        }
        if self.handle.status.is_some() {
            return Err(io::Error::other(
                "child already reaped without resource accounting",
            ));
        }
        if !self.before_reap(block)? {
            return Ok(None);
        }
        let result = rusage::wait(self.id(), block);
        let usage = self.observe_wait_result(result)?;
        if let Some(usage) = usage {
            self.handle.status = Some(usage.status);
            self.usage = Some(usage);
        }
        Ok(usage)
    }

    /// Whether the child has exited, without reaping it: until a wait does, its pid, and the
    /// group and session it leads, stay its own.
    /// # Errors
    /// `waitid`'s, other than `EINTR` (retried); `Unsupported` off Darwin and Linux.
    pub fn exited(&mut self) -> io::Result<bool> {
        self.handle.require_custody()?;
        if self.usage.is_some() || self.handle.status.is_some() {
            return Ok(true);
        }
        let result = rusage::exited(self.id());
        self.observe_wait_result(result)
    }

    /// The child's pid.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.handle.id()
    }

    /// Whether this handle already consumed its child's exit. Its old pid then
    /// supplies no live identity or signalling authority.
    #[must_use]
    pub const fn is_reaped(&self) -> bool {
        self.usage.is_some() || self.handle.status.is_some()
    }

    /// The resource accounting its owning reap collected ([`Self::wait_with_rusage`],
    /// [`Self::try_wait_with_rusage`], an acknowledged exit): `None` before the reap, or
    /// after one that collected none ([`Self::wait`], [`Self::try_wait`]). It waits on nothing.
    #[must_use]
    pub const fn usage(&self) -> Option<Rusage> {
        self.usage
    }

    /// `SIGKILL` the child; once it is reaped, signal nothing and answer `Ok` (the pid may
    /// name another process by then).
    /// # Errors
    /// `kill`'s.
    pub fn kill(&mut self) -> io::Result<()> {
        self.handle.kill()
    }

    /// `SIGTERM` the child, so it can pass the signal on; once it is reaped, signal nothing.
    /// # Errors
    /// `kill`'s.
    pub fn terminate(&mut self) -> io::Result<()> {
        self.handle.terminate()
    }

    /// Run a child started stopped ([`Command::start_suspended`]): `SIGCONT`, which a running child carries on through.
    /// # Errors
    /// `kill`'s.
    pub fn resume(&mut self) -> io::Result<()> {
        self.handle.resume()
    }

    /// Close the child's stdin, then wait for it to exit and reap it (once).
    /// # Errors
    /// `waitpid`'s, other than `EINTR` (retried).
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        drop(self.stdin.take());
        self.before_reap(true)?;
        self.handle.wait()
    }

    /// The exit status if the child has exited (reaping it), else `None`.
    /// # Errors
    /// `waitpid`'s, other than `EINTR` (retried).
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if !self.before_reap(false)? {
            return Ok(None);
        }
        self.handle.try_wait()
    }

    /// Close the child's stdin, read its piped stdout and stderr to their ends (both at once, so neither can fill and stall the child), and wait for it.
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
        // SAFETY: `fds` is a live array of two `struct pollfd`, which `poll` reads and writes and does not retain.
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
    // SAFETY: `F_GETFL`/`F_SETFL` read and set one descriptor's status flags and touch no memory; `fd` is a pipe end owned by the caller.
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

#[cfg(target_vendor = "apple")]
type Handle = super::LaneChild;

#[cfg(target_vendor = "apple")]
impl Command {
    /// Replace this process with the program (`POSIX_SPAWN_SETEXEC`): it holds this process's
    /// stdio and explicitly named guards. This process's own table is untouched if exec fails.
    pub fn exec(&mut self) -> io::Error {
        match self.launch(Fallback::Inherit, true, true, false) {
            Ok(_) => io::Error::other("the exec returned a child"),
            Err(error) => error,
        }
    }

    /// Replace this process and stop the activated image before user code.
    /// The original parent must own and acknowledge the new kernel stop, check
    /// the selected image and policy, and explicitly resume the same child.
    /// This creates no child or resource grant. Ordinary [`Self::exec`] keeps
    /// refusing suspended exec; mapped guards, disclaim and tether also retain
    /// their existing refusals. A failed exec preserves this process's table.
    pub fn exec_suspended(&mut self) -> io::Error {
        self.suspended = true;
        match self.launch(Fallback::Inherit, true, true, true) {
            Ok(_) => io::Error::other("the suspended exec returned a child"),
            Err(error) => error,
        }
    }

    fn start_pending(&mut self, fallback: Fallback, needs_stdin: bool) -> io::Result<PendingChild> {
        let (pid, [stdin, stdout, stderr], status) =
            self.launch(fallback, needs_stdin, false, false)?;
        Ok(PendingChild {
            status,
            child: Child {
                stdin: stdin.map(ChildStdin::from),
                stdout: stdout.map(ChildStdout::from),
                stderr: stderr.map(ChildStderr::from),
                foreground: None,
                handle: super::LaneChild {
                    pid,
                    status: None,
                    custody: super::WaitCustody::default(),
                },
                usage: None,
                custody: Arc::new(()),
            },
        })
    }

    /// The spawn: the child's pid, and this process's ends of its pipes.
    fn launch(
        &mut self,
        fallback: Fallback,
        needs_stdin: bool,
        replace: bool,
        suspended_exec: bool,
    ) -> io::Result<(i32, [Option<OwnedFd>; 3], Option<ExecStatus>)> {
        use super::{
            F_GETFD, F_SETFD, FD_CLOEXEC, Image, Placement, Source, c_string, fcntl, placeable,
            placeable_copy,
        };
        use std::os::unix::ffi::OsStrExt as _;
        if replace
            && self
                .guards
                .iter()
                .any(|guard| guard.source.as_raw_fd() != guard.target)
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "mapped guard exec cannot preserve the failure table",
            ));
        }
        if self.inherit_exec_signal_mask && !replace {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a signal mask is inherited only by an in-place exec",
            ));
        }
        if replace && self.disclaim.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "an exec cannot disclaim responsibility: spawn the disclaimed child instead",
            ));
        }
        if replace && self.suspended && !suspended_exec {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "an exec cannot start suspended: spawn the child instead",
            ));
        }
        if self.tether && (replace || self.suspended) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a tethered child is spawned running: neither an exec nor a suspended start",
            ));
        }
        if self.tether && !self.guards.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "tethered commands do not forward explicit guards",
            ));
        }
        if self.tether && self.native_image {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a tethered command starts its helper, not a native image",
            ));
        }
        let routed = if self.tether {
            crate::tether::route(&self.program, self.arg0.as_deref(), &self.args)
        } else {
            None
        };
        // The tether runs the program only after this spawn started it, so it reports its exec's
        // error back here ([`ExecStatus`]), and the spawn fails with it as a spawn of the program
        // itself would.
        let status = routed.is_some().then(ExecStatus::new).transpose()?;
        let mut env = self.env.capture();
        if let Some(status) = &status {
            use crate::tether::{OWN_PATH_VAR, STATUS_VAR};
            env.retain(|(key, _)| key != STATUS_VAR && key != OWN_PATH_VAR);
            env.push((
                STATUS_VAR.into(),
                status.write.as_raw_fd().to_string().into(),
            ));
            if !self.env.saw_path() {
                env.push((OWN_PATH_VAR.into(), "1".into()));
            }
        }
        let mut image = match &routed {
            Some((exe, args)) => Image::build(exe.as_os_str(), args, &env)?,
            None => Image::build(&self.program, &self.args, &env)?,
        };
        match (&routed, &self.arg0) {
            (Some(_), _) => image.argv[0] = c_string(crate::tether::NAME.as_bytes())?,
            (None, Some(arg0)) => image.argv[0] = c_string(arg0.as_bytes())?,
            (None, None) => {}
        }
        let cwd = self
            .cwd
            .as_ref()
            .map(|dir| c_string(dir.as_os_str().as_bytes()))
            .transpose()?;
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
        let exec_source = |fd| -> io::Result<OwnedFd> {
            let copy = placeable_copy(fd)?;
            // SAFETY: the command exclusively owns this fresh copy throughout the query.
            let flags = unsafe { fcntl(copy.as_raw_fd(), F_GETFD) };
            if flags < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: change only this owned copy. Keep its fork-exclusion mark until
            // spawn_lock::handing lifts it for this dispatch, and clear only CLOEXEC.
            if unsafe { fcntl(copy.as_raw_fd(), F_SETFD, flags & !FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(copy)
        };
        if replace {
            for (target, source) in (0..).zip(sources.iter_mut()) {
                let fd = match *source {
                    Source::Fd(fd) => fd,
                    Source::Inherit => target,
                    Source::Null => continue,
                };
                // SAFETY: `F_GETFD` reads one descriptor's flags; a closed number answers -1 (an inherited slot this process has closed, which the spawn gives `/dev/null`).
                let flags = unsafe { fcntl(fd, F_GETFD) };
                if flags < 0 || flags & FD_CLOEXEC == 0 {
                    continue;
                }
                let copy = exec_source(fd)?;
                *source = Source::Fd(copy.as_raw_fd());
                handed.push(copy);
            }
        }
        let mut guards = Vec::with_capacity(self.guards.len());
        for guard in &self.guards {
            let target = guard.target;
            let source = if replace {
                // SETEXEC cannot inherit a CLOEXEC source directly. Hand a private temporary
                // copy to the original advertised number without changing the parent's slot.
                let copy = exec_source(guard.source.as_raw_fd())?;
                let source = copy.as_raw_fd();
                handed.push(copy);
                source
            } else {
                guard.source.as_raw_fd()
            };
            guards.push((source, target));
        }
        if let Some(status) = &status {
            let end = status.write.as_raw_fd();
            guards.push((end, end));
        }
        let pid = super::platform::spawn(
            &image,
            &Placement {
                stdio: sources,
                lanes: &[],
                guards: &guards,
                cwd: cwd.as_deref(),
                base: base.as_deref(),
                pgroup: self.group(),
                setsid: self.setsid,
                replace,
                long_entry_ends: !self.env.saw_path(),
                inherit_all: self.inherit_all,
                default_signals: self.default_signals,
                inherit_signal_mask: self.inherit_exec_signal_mask,
                native_image: self.native_image,
                disclaim: self.disclaim,
                suspended: self.suspended,
                task: self.task,
                #[cfg(target_os = "macos")]
                footprint: self.footprint,
            },
        )?;
        drop(handed);
        Ok((pid, ours, status))
    }
}

/// A `std` child: on drop collected if it has exited, else handed to
/// [`crate::reaper`].
#[cfg(not(target_vendor = "apple"))]
#[derive(Debug)]
struct Handle {
    child: std::process::Child,
    status: Option<ExitStatus>,
    custody: super::WaitCustody,
}

#[cfg(not(target_vendor = "apple"))]
impl Handle {
    fn require_custody(&self) -> io::Result<()> {
        self.custody.require()
    }

    fn observe_wait_error(&mut self, error: &io::Error) {
        self.custody.observe_error(error, self.status.is_some());
    }
    fn id(&self) -> u32 {
        self.child.id()
    }

    fn kill(&mut self) -> io::Result<()> {
        self.require_custody()?;
        if self.status.is_some() {
            Ok(())
        } else {
            self.child.kill()
        }
    }

    fn terminate(&mut self) -> io::Result<()> {
        self.signal(super::SIGTERM)
    }

    fn resume(&mut self) -> io::Result<()> {
        self.signal(super::SIGCONT)
    }

    fn signal(&mut self, signal: i32) -> io::Result<()> {
        self.require_custody()?;
        if self.status.is_some() {
            return Ok(());
        }
        let pid = i32::try_from(self.id()).map_err(io::Error::other)?;
        // SAFETY: two integers; the pid is our own unreaped child.
        if unsafe { super::kill(pid, signal) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        self.require_custody()?;
        if let Some(status) = self.status {
            return Ok(status);
        }
        let result = self.child.wait();
        if let Err(error) = &result {
            self.observe_wait_error(error);
        }
        let status = result?;
        self.noted(status);
        self.status = Some(status);
        Ok(status)
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.require_custody()?;
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        let result = self.child.try_wait();
        if let Err(error) = &result {
            self.observe_wait_error(error);
        }
        self.status = result?;
        if let Some(status) = self.status {
            self.noted(status);
        }
        Ok(self.status)
    }

    /// Hand a status `std`'s wait just reaped to the death journal ([`crate::deaths`]).
    fn noted(&self, status: ExitStatus) {
        crate::deaths::note(
            i32::try_from(self.id()).unwrap_or(0),
            std::os::unix::process::ExitStatusExt::into_raw(status),
        );
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

#[cfg(not(target_vendor = "apple"))]
unsafe extern "C" {
    fn sigprocmask(how: i32, set: *const u64, old: *mut u64) -> i32;
    #[link_name = "setsid"]
    fn new_session() -> i32;
}
#[cfg(not(target_vendor = "apple"))]
const SIG_SETMASK: i32 = 2;

/// A routed spawn's exec status ([`crate::tether::STATUS_VAR`]): a pipe whose write end the spawn
/// hands its tether, open across that exec alone, and whose read end tells the program's exec:
/// its end once every write end closed (this process's after the spawn, the tether's at the
/// program's exec, its watcher's as it arms), or the four bytes of the `errno` its exec failed
/// with. Elsewhere there is none.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
struct ExecStatus {
    read: std::io::PipeReader,
    write: OwnedFd,
}

/// No routed spawn has an exec status off macOS and Linux.
#[cfg(all(not(target_vendor = "apple"), not(target_os = "linux")))]
type ExecStatus = std::convert::Infallible;

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
impl ExecStatus {
    /// A fresh pipe, both ends close-on-exec in this process, the write end above stdio: the
    /// spawn hands that number on as it is, and the child's stdio, placed over 0 to 2, would
    /// replace a write end there, or the program would inherit it as stdio when stdio is
    /// inherited. Neither end is kept out of other children ([`crate::kept_out`]): close-on-exec
    /// already keeps both out of every other spawn's program, and a mark would make the spawn
    /// that hands the write end hold the spawn lock exclusively ([`crate::spawn_lock::handing`]),
    /// as its creation would, so each tethered spawn would wait for every spawn in flight and
    /// stall every later one behind it. A fork that does not exec (only tests fork here) holds a
    /// copy until it ends, which delays the status's end, as it does on Linux.
    fn new() -> io::Result<Self> {
        use std::os::fd::FromRawFd as _;
        let (read, write) = std::io::pipe()?;
        let mut write = OwnedFd::from(write);
        if write.as_raw_fd() < 3 {
            // SAFETY: `fcntl` duplicates a descriptor this process owns onto the lowest free
            // number from 3; the result is a fresh descriptor nothing else owns, or -1.
            let above = unsafe { super::fcntl(write.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
            if above < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: as above, the descriptor the call just made.
            write = unsafe { OwnedFd::from_raw_fd(above) };
        }
        Ok(Self { read, write })
    }

    /// Once the tether was started: this process's write end closed, the read end read to its end.
    /// `Ok` when the program's exec closed the tether's (or the tether ended before it, which its
    /// status tells); its exec's error when the tether wrote one.
    fn after_fork(self) -> io::Result<()> {
        let Self { mut read, write } = self;
        drop(write);
        let mut said = Vec::with_capacity(4);
        loop {
            match read.read_to_end(&mut said) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
                Ok(_) => break,
            }
        }
        #[cfg(all(target_os = "macos", debug_assertions))]
        exec_status_control::after_read()?;
        match <[u8; 4]>::try_from(said.as_slice()) {
            Ok(errno) => Err(io::Error::from_raw_os_error(i32::from_ne_bytes(errno))),
            Err(_) if said.is_empty() => Ok(()),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the tether's exec status read {} bytes", said.len()),
            )),
        }
    }
}

#[cfg(not(target_vendor = "apple"))]
impl Command {
    /// This platform has no qualified same-process suspended exec boundary.
    pub fn exec_suspended(&mut self) -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "suspended exec requires the native Apple launch boundary",
        )
    }
    /// Replace this process with the program: [`super::InheritStrip`] marks this process's own table first, so a failed exec leaves every descriptor close-on-exec ([`super::InheritNothing`] says what that means). Explicit guards are refused before that strip: their fork-only inheritance hook cannot safely modify this process's table on an exec failure.
    pub fn exec(&mut self) -> io::Error {
        use std::os::unix::process::CommandExt as _;
        if !self.guards.is_empty() {
            return io::Error::new(
                io::ErrorKind::Unsupported,
                "explicit guards require a spawned child on this platform",
            );
        }
        if self.tether {
            return io::Error::new(
                io::ErrorKind::Unsupported,
                "a tethered child is spawned, never exec'd in this process's place",
            );
        }
        match self.forked() {
            Ok((mut command, _)) => command.exec(),
            Err(error) => error,
        }
    }

    fn start_pending(&mut self, fallback: Fallback, needs_stdin: bool) -> io::Result<PendingChild> {
        super::note_spawn();
        let (mut command, status) = self.forked()?;
        #[cfg(target_os = "linux")]
        let routed = status.is_some();
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
        // Each try is one fork; the pause between tries runs here, on the calling thread, so a
        // fork that runs on a spawner holds it for that fork alone.
        #[cfg(target_os = "linux")]
        let on_a_spawner = self.forks_on_the_spawner();
        let command = std::sync::Arc::new(std::sync::Mutex::new(command));
        let mut child = super::past_text_busy(
            super::TEXT_BUSY_FOR,
            || {
                let command = std::sync::Arc::clone(&command);
                let fork = move || {
                    let mut command = command
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    // The tether's watcher watches the thread that forks it, as the parent-death
                    // signal follows it (`crate::tether::THREAD_VAR`).
                    #[cfg(target_os = "linux")]
                    if routed {
                        // SAFETY: `gettid` takes nothing and cannot fail.
                        let thread = unsafe { libc::syscall(libc::SYS_gettid) };
                        command.env(crate::tether::THREAD_VAR, thread.to_string());
                    }
                    command.spawn()
                };
                #[cfg(target_os = "linux")]
                if on_a_spawner {
                    return crate::tether::spawned(fork);
                }
                fork()
            },
            super::text_busy_pause,
        )?;
        Ok(PendingChild {
            status,
            child: Child {
                stdin: child.stdin.take(),
                stdout: child.stdout.take(),
                stderr: child.stderr.take(),
                foreground: None,
                handle: Handle {
                    child,
                    status: None,
                    custody: super::WaitCustody::default(),
                },
                usage: None,
                custody: Arc::new(()),
            },
        })
    }

    /// Whether the fork runs on one of the tether's spawners (`crate::tether::spawned`), threads
    /// that live as long as this process, so the parent-death signal `bind` sets comes at this
    /// process's death, not at the calling thread's: a tethered spawn's does, but one a test ties
    /// to its own thread (`tie_to_this_thread`).
    #[cfg(target_os = "linux")]
    fn forks_on_the_spawner(&self) -> bool {
        #[cfg(test)]
        let this_thread = self.this_thread;
        #[cfg(not(test))]
        let this_thread = false;
        self.tether && !this_thread
    }

    /// The `std` command this one configures: its explicit descriptor policy first, then
    /// the empty mask and session in one more hook; and, for one that runs through its tether
    /// ([`Self::route`]), the exec status it hands that tether ([`ExecStatus`]).
    fn forked(&self) -> io::Result<(super::StdCommand, Option<ExecStatus>)> {
        use super::InheritNothing as _;
        use std::os::unix::process::CommandExt as _;
        if self
            .guards
            .iter()
            .any(|guard| guard.source.as_raw_fd() != guard.target)
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "mapped guards need native spawn actions",
            ));
        }
        if self.disclaim.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "disclaiming responsibility is Darwin's: no child is started without it",
            ));
        }
        if self.suspended {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "starting a child stopped at its exec is Darwin's: no child is started",
            ));
        }
        if self.tether && !self.guards.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "tethered commands do not forward explicit guards",
            ));
        }
        #[cfg(target_os = "linux")]
        let routed = self.route();
        #[cfg(not(target_os = "linux"))]
        let routed: Option<(std::path::PathBuf, Vec<OsString>)> = None;
        #[expect(
            clippy::disallowed_types,
            reason = "spawnfd's own Command, with its explicit descriptor policy installed before the native process hook"
        )]
        let mut command = std::process::Command::new(
            routed
                .as_ref()
                .map_or(self.program.as_os_str(), |(exe, _)| exe.as_os_str()),
        );
        if self.inherit_all {
            // Trust's std strips every descriptor above stdio from a child unless told not to;
            // upstream's std hands on the whole non-close-on-exec table by itself.
            #[cfg(trust_std)]
            command.inherit_all_fds();
        } else {
            command.inherit_nothing();
        }
        match &routed {
            Some((_, args)) => {
                command.args(args).arg0(crate::tether::NAME);
            }
            None => {
                command.args(&self.args);
                if let Some(arg0) = &self.arg0 {
                    command.arg0(arg0);
                }
            }
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
        #[cfg(target_os = "linux")]
        let status = routed.is_some().then(ExecStatus::new).transpose()?;
        #[cfg(target_os = "linux")]
        if let Some(status) = &status {
            command.env(
                crate::tether::STATUS_VAR,
                status.write.as_raw_fd().to_string(),
            );
        }
        #[cfg(target_os = "linux")]
        let status_end = status.as_ref().map(|status| status.write.as_raw_fd());
        #[cfg(not(target_os = "linux"))]
        let status: Option<ExecStatus> = None;
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
        let default_signals = self.default_signals;
        let guards: Vec<_> = self.guard_targets().collect();
        #[cfg(target_os = "linux")]
        let tether = self
            .tether
            .then(|| i32::try_from(std::process::id()))
            .transpose()
            .map_err(io::Error::other)?;
        // SAFETY: the hook runs between fork and exec and calls only signal/session operations,
        // the tether's prctl/getppid, and fcntl over pre-captured descriptors. Command owns
        // those occupied guard numbers throughout this private forked command's spawn/exec, and
        // the caller the exec status's write end until the fork returned; no allocation, lock or
        // descriptor replacement occurs in the child hook.
        unsafe {
            command.pre_exec(move || {
                if default_signals {
                    // SAFETY: a zeroed action with an empty mask and SIG_DFL holds no Rust
                    // callback; sigaction is async-signal-safe. Linux exposes signals 1..=64;
                    // KILL/STOP cannot be changed, and libc reserves 32/33 for its threads.
                    let mut action: libc::sigaction = std::mem::zeroed();
                    action.sa_sigaction = libc::SIG_DFL;
                    libc::sigemptyset(&mut action.sa_mask);
                    for sig in 1..=64 {
                        if sig == libc::SIGKILL || sig == libc::SIGSTOP {
                            continue;
                        }
                        if libc::sigaction(sig, &action, std::ptr::null_mut()) != 0 {
                            let error = io::Error::last_os_error();
                            if !matches!(sig, 32 | 33) || error.raw_os_error() != Some(libc::EINVAL)
                            {
                                return Err(error);
                            }
                        }
                    }
                }
                let empty = [0u64; 16];
                sigprocmask(SIG_SETMASK, empty.as_ptr(), std::ptr::null_mut());
                if setsid && new_session() < 0 {
                    return Err(io::Error::last_os_error());
                }
                #[cfg(target_os = "linux")]
                let handed = guards.iter().copied().chain(status_end);
                #[cfg(not(target_os = "linux"))]
                let handed = guards.iter().copied();
                for guard in handed {
                    let flags = super::fcntl(guard, super::F_GETFD);
                    if flags < 0
                        || super::fcntl(guard, super::F_SETFD, flags & !super::FD_CLOEXEC) < 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                }
                #[cfg(target_os = "linux")]
                if let Some(parent) = tether {
                    crate::tether::bind(parent)?;
                }
                Ok(())
            });
        }
        if self.native_image {
            self.native_hook(&mut command)?;
        }
        Ok((command, status))
    }

    /// The tether a Linux tethered spawn runs through ([`crate::tether::route`]): one whose child
    /// leads its own process group (`process_group(0)`, or a session), whose tether leaves a
    /// watcher that ends that group, while this process records an image to run through
    /// (`crate::tether::route` says when it does not). `None` for any other spawn, whose child is
    /// tied by its parent-death signal alone. The tether runs the program only after it forked
    /// that watcher, so it reports its exec's error on an [`ExecStatus`], and the spawn fails
    /// with it as a spawn of the program itself would.
    #[cfg(target_os = "linux")]
    fn route(&self) -> Option<(std::path::PathBuf, Vec<OsString>)> {
        if !self.tether || !(self.setsid || self.group() == Some(0)) {
            return None;
        }
        crate::tether::route(&self.program, self.arg0.as_deref(), &self.args)
    }

    /// The last hook of a native-image spawn ([`super::NativeImage`]): the child `execve`s the
    /// image prepared here, and a failed exec fails the spawn with its error, before `std`'s
    /// `execvp` could hand the file to the system shell.
    #[cfg(target_os = "linux")]
    fn native_hook(&self, command: &mut super::StdCommand) -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::process::CommandExt as _;
        if self.tether {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a tethered command is not started as a native image",
            ));
        }
        // The search runs here, before the fork, against this process's directory, where the
        // child's `execvp` would search against the one it moved to.
        if self.cwd.is_some() && !self.program.as_bytes().contains(&b'/') {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a native image started in another directory is named by its path",
            ));
        }
        let native = super::platform::NativeExec::prepare(
            &self.program,
            self.arg0.as_deref(),
            &self.args,
            &self.env.capture(),
        )?;
        // SAFETY: added after every other hook, it runs last between fork and exec, once stdio,
        // the working directory, the strip, the mask, the session and the guards are in place,
        // and calls only `NativeExec::exec` (an `execve` over what `native` owns, then errno).
        unsafe {
            command.pre_exec(move || Err(native.exec()));
        }
        Ok(())
    }

    /// A native-image spawn is macOS's and Linux's ([`super::NativeImage`]).
    #[cfg(not(target_os = "linux"))]
    fn native_hook(&self, _command: &mut super::StdCommand) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a native-image spawn is macOS's and Linux's",
        ))
    }
}

#[cfg(all(target_os = "macos", debug_assertions))]
#[path = "spawnfd_exec_status_control.rs"]
mod exec_status_control;

/// Runs the native fixture for exec-status observation failure after proven target execution.
/// This debug-only control creates no admission or execution authority.
#[cfg(all(target_os = "macos", debug_assertions))]
#[doc(hidden)]
pub fn exec_status_observation_control(world: &std::path::Path) {
    exec_status_control::run(world);
}

/// Actual native child ownership exists before status observation can fail.
pub(super) struct PendingChild {
    child: Child,
    status: Option<ExecStatus>,
}

impl PendingChild {
    pub(super) fn observe<R>(self, seal: impl FnOnce(Child) -> R) -> Result<R, (R, io::Error)> {
        let Self { child, status } = self;
        let owned = seal(child);
        #[cfg(any(target_vendor = "apple", target_os = "linux"))]
        if let Some(status) = status
            && let Err(error) = status.after_fork()
        {
            return Err((owned, error));
        }
        #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
        let _ = status;
        Ok(owned)
    }
}

impl Command {
    fn start(&mut self, fallback: Fallback, needs_stdin: bool) -> io::Result<Child> {
        match self
            .start_pending(fallback, needs_stdin)?
            .observe(|child| child)
        {
            Ok(child) => Ok(child),
            Err((mut child, error)) => {
                // Ordinary spawning supplies no cancellation receipt. Keep its established
                // owning wait before reporting the target's exec-status error.
                let _waited = child.wait();
                Err(error)
            }
        }
    }
}

/// TEST SEAM: observe one actual command launch with a status fault only after the target's
/// fixed execution proof. This changes no admission, command, receipt or settlement policy.
/// The thread-local fault is restored on return or unwind; the callback still uses the normal
/// native Command and its actual original resource owner.
#[cfg(all(target_os = "macos", debug_assertions))]
#[doc(hidden)]
pub fn with_exec_status_observation_fault_for_tests<R>(
    proof: &Path,
    command: &mut Command,
    launch: impl FnOnce(&mut Command) -> R,
) -> R {
    exec_status_control::with_fault(proof, || launch(command))
}
