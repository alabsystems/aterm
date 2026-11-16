// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kernel observations for a cooperative exec account move. This is not a
//! supervisor grant: the host must still prove its peer, foreground and claim,
//! check live quota through the shared pool, and retain native mutation custody.

use super::ExecCommand;
use crate::harness::native_act::{Image, Process};
use crate::harness::{codex_alab, self_claimed, upgrade::ArgvRefusal};
use aterm_uds::process::Lineage;
use atpkg::caller_shell::ProcArgs;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Eq)]
pub struct ExecSource {
    pub process: Process,
    /// Kernel exec generation, where the platform exposes it.
    pub exec_generation: Option<i32>,
    /// The process's unique id, read with its birth ([`aterm_uds::process::Lineage::uniqueid`]):
    /// `p_uniqueid` on Apple, the start tick on Linux; 0 in a source no kernel read made
    /// ([`Self::observed`]).
    pub uniqueid: u64,
    pub config_home: PathBuf,
    pub auth_path: PathBuf,
    pub cwd: PathBuf,
    argv: Vec<String>,
}

impl std::fmt::Debug for ExecSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Arguments may contain a private prompt. No environment or argv is logged.
        f.debug_struct("ExecSource")
            .field("pid", &self.process.pid)
            .field("birth", &self.process.birth)
            .field("exec_generation", &self.exec_generation)
            .field("uniqueid", &self.uniqueid)
            .field("image", &self.process.image)
            .finish_non_exhaustive()
    }
}

/// Why a process has no [`ExecSource`] ([`ExecSource::classify`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMiss {
    /// Its argv is not a Codex `exec` run's: no `exec` or `e` subcommand.
    NotExec,
    /// An exec run that no read of it can move: a pid or birth of 0, a relative working
    /// directory, an image path that is not UTF-8 or no longer names its image, a launch variable
    /// (`HOME`, `CODEX_HOME`, `CODEX_AUTH_PATH`) set twice, no Codex home, a relative auth path,
    /// or an invocation the command codec refuses ([`ExecSource::command_for`]).
    Unsupported,
    /// The kernel did not answer a read, or two reads of it differ: a later read may answer.
    /// Off Apple, Linux and Android no lineage reader is wired
    /// ([`aterm_uds::process::read_lineage`] is `None` there), so every read is this one and no
    /// later read answers: a caller that waits on it judges first whether its host can move an
    /// exec run at all.
    Unread,
}

/// One birth-bracketed read of a process: what it observed, and what it judged that to be.
/// `None` is [`CaptureMiss::Unread`].
type Read = Option<(Process, Result<ExecSource, CaptureMiss>)>;

/// The kernel's answers one [`ExecSource::read`] asks for: [`Live`]'s, or a test's script.
trait Kernel {
    fn process(&mut self, pid: u32) -> Option<Process>;
    fn lineage(&mut self, pid: i32) -> Option<Lineage>;
    fn args(&mut self, pid: u32) -> Option<ProcArgs>;
    fn cwd(&mut self, pid: u32) -> Option<PathBuf>;
}

/// This machine's kernel.
struct Live;

impl Kernel for Live {
    fn process(&mut self, pid: u32) -> Option<Process> {
        Process::observe(pid)
    }

    fn lineage(&mut self, pid: i32) -> Option<Lineage> {
        aterm_uds::process::read_lineage(pid)
    }

    fn args(&mut self, pid: u32) -> Option<ProcArgs> {
        atpkg::caller_shell::process_args(pid)
    }

    fn cwd(&mut self, pid: u32) -> Option<PathBuf> {
        aterm_sysprobe::process_cwd(pid)
    }
}

impl ExecSource {
    /// Capture a stable kernel snapshot, without a TUI session record or a
    /// caller-supplied command. A managed-image peer proof is required separately.
    #[must_use]
    pub fn capture(pid: u32) -> Option<Self> {
        Self::classify(pid).ok()
    }

    /// [`Self::capture`], saying why there is none. Two reads of `pid` must agree, on the process
    /// and on its judgment, for either a source or a miss other than [`CaptureMiss::Unread`].
    ///
    /// # Errors
    /// [`CaptureMiss::Unread`] where a kernel read fails or the two reads differ; otherwise the
    /// first read's [`CaptureMiss::NotExec`] or [`CaptureMiss::Unsupported`].
    pub fn classify(pid: u32) -> Result<Self, CaptureMiss> {
        Self::judged(|| Self::read(&mut Live, pid))
    }

    /// [`Self::classify`] over two calls of `read`.
    fn judged(mut read: impl FnMut() -> Read) -> Result<Self, CaptureMiss> {
        let (process, judged) = read().ok_or(CaptureMiss::Unread)?;
        let again = read().ok_or(CaptureMiss::Unread)?;
        if again.0 != process || again.1 != judged {
            return Err(CaptureMiss::Unread);
        }
        judged
    }

    fn read(kernel: &mut impl Kernel, pid: u32) -> Read {
        let process = kernel.process(pid)?;
        let lineage = kernel.lineage(i32::try_from(pid).ok()?)?;
        if lineage.identity.birth.0 != process.birth {
            return None;
        }
        let args = kernel.args(pid)?;
        let cwd = kernel.cwd(pid)?;
        let judged = Self::from_observations(process.clone(), args, cwd).map(|mut source| {
            source.exec_generation = lineage.identity.idversion;
            source.uniqueid = lineage.uniqueid;
            source
        });
        // A judgment holds only of the process both brackets read.
        let same = kernel.process(pid).as_ref() == Some(&process)
            && kernel.lineage(lineage.identity.pid).is_some_and(|now| {
                now.identity == lineage.identity && now.uniqueid == lineage.uniqueid
            });
        same.then_some((process, judged))
    }

    fn from_observations(
        process: Process,
        args: ProcArgs,
        cwd: PathBuf,
    ) -> Result<Self, CaptureMiss> {
        use CaptureMiss::Unsupported;
        // Reuse the existing parser only to identify exec. Its intentional exec
        // refusal keeps this source out of the interactive restart contract.
        if !matches!(codex_alab::rewrite_argv(&args.argv, None),
            Err(ArgvRefusal::NotResumable(mode)) if mode == "exec" || mode == "e")
        {
            return Err(CaptureMiss::NotExec);
        }
        if process.pid == 0 || process.birth == 0 || !cwd.is_absolute() {
            return Err(Unsupported);
        }
        let program = std::str::from_utf8(&process.image.path).map_err(|_| Unsupported)?;
        if Image::at(Path::new(program)).as_ref() != Some(&process.image) {
            return Err(Unsupported);
        }
        let config_home = self_claimed::codex_home_of(
            unique_env(&args, "CODEX_HOME").ok_or(Unsupported)?,
            Some(&cwd),
            unique_env(&args, "HOME").ok_or(Unsupported)?,
        )
        .ok_or(Unsupported)?;
        let auth_path = match unique_env(&args, "CODEX_AUTH_PATH")
            .ok_or(Unsupported)?
            .filter(|s| !s.is_empty())
        {
            Some(value) => PathBuf::from(value),
            None => config_home.join("auth.json"),
        };
        if !auth_path.is_absolute() {
            return Err(Unsupported);
        }
        let source = Self {
            process,
            exec_generation: None,
            uniqueid: 0,
            config_home,
            auth_path,
            cwd,
            argv: args.argv,
        };
        // Apply the exact command codec's bounds and NUL checks at capture too.
        source.command_for(&source.auth_path).ok_or(Unsupported)?;
        Ok(source)
    }

    /// The source a capture of `process` would give from its `args` and `cwd`
    /// ([`Self::capture`]), with no kernel read, no exec generation and no unique id: for a test
    /// that keeps sources of processes it does not run as Codex, which need be no live ones. Its
    /// [`Self::current`] is false, so [`Self::prepare_on`] answers `exec-source-changed` once its
    /// handoff has run; what that handoff read and recorded is the test's to judge.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn observed(process: Process, args: ProcArgs, cwd: PathBuf) -> Option<Self> {
        Self::from_observations(process, args, cwd).ok()
    }

    /// Prepare through the existing account pool, including its fresh source and
    /// destination usage reads, machine claims and login generations. There is no
    /// shell command or shell dialect in this session's launch representation.
    /// The handoff leaves what the session's `intent` asks
    /// ([`crate::harness::codex_rotation::Ask`]): its source's wall, of the codex bucket or of the
    /// limit the intent names, so no exec prepare drops the limit it was asked about. Its reads
    /// are paced by `pace`, the asking process's ([`crate::harness::codex_rotation::prepare`]).
    pub fn prepare(
        &self,
        seat: &crate::harness::accounts_pool::Seat<'_>,
        sid: &str,
        intent: &crate::harness::codex_renewal::Intent,
        reader: &dyn crate::harness::codex_rotation::UsageReader,
        wait: std::time::Duration,
        pace: &mut crate::harness::codex_rotation::ReadPace,
    ) -> Result<Option<crate::harness::codex_rotation::Proposal<ExecCommand>>, &'static str> {
        if !self.current() {
            return Err("exec-source-changed");
        }
        let program = std::str::from_utf8(&self.process.image.path).map_err(|_| "exec-program")?;
        self.prepare_on(seat, sid, intent, reader, wait, pace, Path::new(program))
    }

    /// [`Self::prepare`], judging the Codex `program` this session execs.
    #[expect(
        clippy::too_many_arguments,
        reason = "prepare's five, the asking process's pace and the program it execs"
    )]
    pub(crate) fn prepare_on(
        &self,
        seat: &crate::harness::accounts_pool::Seat<'_>,
        sid: &str,
        intent: &crate::harness::codex_renewal::Intent,
        reader: &dyn crate::harness::codex_rotation::UsageReader,
        wait: std::time::Duration,
        pace: &mut crate::harness::codex_rotation::ReadPace,
        program: &Path,
    ) -> Result<Option<crate::harness::codex_rotation::Proposal<ExecCommand>>, &'static str> {
        use crate::harness::{accounts, accounts_pool, codex_rotation};
        let default_home = seat.home.join(accounts::Cli::Codex.accounts().default_home);
        let env = self.account_env(&default_home).ok_or("exec-auth-path")?;
        let session = accounts_pool::Session {
            cli: accounts::Cli::Codex,
            sid,
            supervisor: accounts::Supervisor::Own,
            env: env.as_slice(),
            seen: &crate::harness::observe::Observation::Unread,
            sample: None,
            point: codex_rotation::wall_asked(seat.now),
            mode: seat.mode_for(accounts::Cli::Codex),
            relaunch: (),
        };
        let result = codex_rotation::prepare_exec(
            seat,
            &session,
            program,
            intent,
            reader,
            wait,
            pace,
            |row| {
                let home = row.dir.as_ref().map(PathBuf::from).unwrap_or_else(|| {
                    seat.home.join(accounts::Cli::Codex.accounts().default_home)
                });
                self.command_for(&home.join("auth.json"))
            },
        );
        if !self.current() {
            return Err("exec-source-changed");
        }
        result
    }

    fn account_env(&self, default_home: &Path) -> Option<Vec<(&str, &str)>> {
        // Attribute the login through its proven credential pointer, as TUI
        // rotation does. CODEX_HOME belongs to config/history; presenting it as
        // a second login override makes the shared pool refuse a managed row.
        // The provider reader separately retains the kernel-bound config home.
        // This machine's own login is the row with no directory, which a pointer
        // to it would not name: a session there carries none, as a TUI's does.
        if self.auth_path == default_home.join("auth.json") {
            return Some(Vec::new());
        }
        Some(vec![("CODEX_AUTH_PATH", self.auth_path.to_str()?)])
    }

    /// Recheck after provider I/O and before native admission. A changed image,
    /// process generation, argv, directory or effective auth pointer revokes it.
    #[must_use]
    pub fn current(&self) -> bool {
        Self::capture(self.process.pid).as_ref() == Some(self)
    }

    /// The original invocation with a separately selected account path. The
    /// native host must verify that path against the shared pool's chosen row.
    #[must_use]
    pub fn command_for(&self, selected_auth: &Path) -> Option<ExecCommand> {
        let command = ExecCommand {
            image: self.process.image.clone(),
            program: std::str::from_utf8(&self.process.image.path).ok()?.into(),
            argv0: self.argv.first()?.clone(),
            argv: self.argv.get(1..)?.to_vec(),
            auth_path: selected_auth.to_str()?.into(),
            cwd: Some(self.cwd.to_str()?.into()),
        };
        command.encode()?;
        Some(command)
    }
}

// ProcArgs::env_var returns the first match. An authority boundary cannot
// silently choose between two conflicting launch values.
fn unique_env<'a>(args: &'a ProcArgs, key: &str) -> Option<Option<&'a str>> {
    let prefix = format!("{key}=");
    let mut entries = args.env.iter().filter_map(|v| v.strip_prefix(&prefix));
    let value = entries.next();
    entries.next().is_none().then_some(value)
}

#[cfg(test)]
#[path = "codex_exec_source_tests.rs"]
mod tests;
