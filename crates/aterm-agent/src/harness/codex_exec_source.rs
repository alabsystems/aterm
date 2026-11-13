// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kernel observations for a cooperative exec account move. This is not a
//! supervisor grant: the host must still prove its peer, foreground and claim,
//! check live quota through the shared pool, and retain native mutation custody.

use super::ExecCommand;
use crate::harness::native_act::{Image, Process};
use crate::harness::{codex_alab, self_claimed, upgrade::ArgvRefusal};
use atpkg::caller_shell::ProcArgs;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Eq)]
pub struct ExecSource {
    pub process: Process,
    /// Kernel exec generation, where the platform exposes it.
    pub exec_generation: Option<i32>,
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
            .field("image", &self.process.image)
            .finish_non_exhaustive()
    }
}

impl ExecSource {
    /// Capture a stable kernel snapshot, without a TUI session record or a
    /// caller-supplied command. A managed-image peer proof is required separately.
    #[must_use]
    pub fn capture(pid: u32) -> Option<Self> {
        let before = Self::read(pid)?;
        (Self::read(pid).as_ref() == Some(&before)).then_some(before)
    }

    fn read(pid: u32) -> Option<Self> {
        let process = Process::observe(pid)?;
        let identity = aterm_uds::process::read_identity(i32::try_from(pid).ok()?)?;
        if identity.birth.0 != process.birth {
            return None;
        }
        let args = atpkg::caller_shell::process_args(pid)?;
        let cwd = aterm_sysprobe::process_cwd(pid)?;
        let mut source = Self::from_observations(process, args, cwd)?;
        source.exec_generation = identity.idversion;
        (Process::observe(pid).as_ref() == Some(&source.process)
            && aterm_uds::process::read_identity(identity.pid) == Some(identity))
        .then_some(source)
    }

    fn from_observations(process: Process, args: ProcArgs, cwd: PathBuf) -> Option<Self> {
        if process.pid == 0 || process.birth == 0 || !cwd.is_absolute() {
            return None;
        }
        // Reuse the existing parser only to identify exec. Its intentional exec
        // refusal keeps this source out of the interactive restart contract.
        if !matches!(codex_alab::rewrite_argv(&args.argv, None),
            Err(ArgvRefusal::NotResumable(mode)) if mode == "exec" || mode == "e")
        {
            return None;
        }
        let program = std::str::from_utf8(&process.image.path).ok()?;
        if Image::at(Path::new(program)).as_ref() != Some(&process.image) {
            return None;
        }
        let config_home = self_claimed::codex_home_of(
            unique_env(&args, "CODEX_HOME")?,
            Some(&cwd),
            unique_env(&args, "HOME")?,
        )?;
        let auth_path = match unique_env(&args, "CODEX_AUTH_PATH")?.filter(|s| !s.is_empty()) {
            Some(value) => PathBuf::from(value),
            None => config_home.join("auth.json"),
        };
        if !auth_path.is_absolute() {
            return None;
        }
        let source = Self {
            process,
            exec_generation: None,
            config_home,
            auth_path,
            cwd,
            argv: args.argv,
        };
        // Apply the exact command codec's bounds and NUL checks at capture too.
        source.command_for(&source.auth_path)?;
        Some(source)
    }

    /// Prepare through the existing account pool, including its fresh source and
    /// destination usage reads, machine claims and login generations. There is no
    /// shell command or shell dialect in this session's launch representation.
    pub fn prepare(
        &self,
        seat: &crate::harness::accounts_pool::Seat<'_>,
        sid: &str,
        expected_source: &crate::harness::codex_renewal::Identity,
        reader: &dyn crate::harness::codex_rotation::UsageReader,
        wait: std::time::Duration,
    ) -> Result<Option<crate::harness::codex_rotation::Proposal<ExecCommand>>, &'static str> {
        if !self.current() {
            return Err("exec-source-changed");
        }
        let program = std::str::from_utf8(&self.process.image.path).map_err(|_| "exec-program")?;
        self.prepare_on(seat, sid, expected_source, reader, wait, Path::new(program))
    }

    /// [`Self::prepare`], judging the Codex `program` this session execs.
    pub(crate) fn prepare_on(
        &self,
        seat: &crate::harness::accounts_pool::Seat<'_>,
        sid: &str,
        expected_source: &crate::harness::codex_renewal::Identity,
        reader: &dyn crate::harness::codex_rotation::UsageReader,
        wait: std::time::Duration,
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
            expected_source,
            reader,
            wait,
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
