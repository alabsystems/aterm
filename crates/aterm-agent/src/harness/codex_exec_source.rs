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
        use crate::harness::{accounts, accounts_pool, codex_rotation};
        if !self.current() {
            return Err("exec-source-changed");
        }
        let config = self.config_home.to_str().ok_or("exec-config-path")?;
        let auth = self.auth_path.to_str().ok_or("exec-auth-path")?;
        let env = [("CODEX_HOME", config), ("CODEX_AUTH_PATH", auth)];
        let session = accounts_pool::Session {
            cli: accounts::Cli::Codex,
            sid,
            supervisor: accounts::Supervisor::Own,
            env: &env,
            sample: None,
            point: accounts::Point::Wall {
                window: "seven_day".into(),
                at: Some(seat.now),
            },
            mode: seat.mode_for(accounts::Cli::Codex),
            relaunch: (),
        };
        let result =
            codex_rotation::prepare_exec(seat, &session, expected_source, reader, wait, |row| {
                let home = row.dir.as_ref().map(PathBuf::from).unwrap_or_else(|| {
                    seat.home.join(accounts::Cli::Codex.accounts().default_home)
                });
                self.command_for(&home.join("auth.json"))
            });
        if !self.current() {
            return Err("exec-source-changed");
        }
        result
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
mod tests {
    use super::*;

    fn facts(root: &Path) -> (Process, ProcArgs) {
        let image = Image::at(&std::env::current_exe().unwrap()).unwrap();
        let process = Process {
            pid: 123,
            birth: 456,
            parent: 10,
            group: 123,
            session: 10,
            image,
        };
        let args = ProcArgs {
            exec_path: String::from_utf8(process.image.path.clone()).unwrap(),
            argv: vec![
                "original-codex-name".into(),
                "exec".into(),
                "--json".into(),
                "-".into(),
                "line one\nline two".into(),
            ],
            env: vec![
                format!("HOME={}", root.display()),
                format!("CODEX_AUTH_PATH={}/source/auth.json", root.display()),
                "UNRELATED_SECRET=must-not-be-retained".into(),
            ],
        };
        (process, args)
    }

    #[test]
    fn native_exec_source_retains_the_whole_invocation_and_only_auth_paths() {
        let root = aterm_tempfile::tempdir().unwrap();
        let (process, args) = facts(root.path());
        let expected = args.argv.clone();
        let source = ExecSource::from_observations(process, args, root.path().into()).unwrap();
        let target = root.path().join("other/auth.json");
        let command = source.command_for(&target).unwrap();
        assert_eq!(command.argv0, expected[0]);
        assert_eq!(command.argv, expected[1..]);
        assert_eq!(command.auth_path, target.to_str().unwrap());
        assert_eq!(source.auth_path, root.path().join("source/auth.json"));
        assert_eq!(source.config_home, root.path().join(".codex"));
        let debug = format!("{source:?}");
        assert!(!debug.contains("line one"));
        assert!(!debug.contains("UNRELATED_SECRET"));
    }

    #[test]
    fn native_exec_source_rejects_other_modes_changed_image_and_ambiguous_environment() {
        let root = aterm_tempfile::tempdir().unwrap();
        let (process, args) = facts(root.path());
        for mode in ["resume", "review", "login"] {
            let mut changed = args.clone();
            changed.argv[1] = mode.into();
            assert!(
                ExecSource::from_observations(process.clone(), changed, root.path().into())
                    .is_none()
            );
        }
        let mut changed = process.clone();
        changed.image.inode += 1;
        assert!(ExecSource::from_observations(changed, args.clone(), root.path().into()).is_none());
        for key in ["HOME", "CODEX_HOME", "CODEX_AUTH_PATH"] {
            let mut changed = args.clone();
            changed.env.push(format!("{key}=/first"));
            changed.env.push(format!("{key}=/second"));
            assert!(
                ExecSource::from_observations(process.clone(), changed, root.path().into())
                    .is_none()
            );
        }
        let mut changed = args;
        changed.env.retain(|v| !v.starts_with("CODEX_AUTH_PATH="));
        changed
            .env
            .push("CODEX_AUTH_PATH=relative/auth.json".into());
        assert!(ExecSource::from_observations(process, changed, root.path().into()).is_none());
    }
}
