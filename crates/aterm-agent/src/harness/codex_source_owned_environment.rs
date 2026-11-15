// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit complete launch environment DATA, never SDK admission.
//! Native owned children must not silently inherit the GUI's environment.
use super::*;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;

/// A private copy of explicitly requested launch settings for an observed process. Debug
/// redacts its values and no serialization is implemented. It is not a process lease or a
/// credential/provider grant; the child still needs its independent constructor.
#[derive(Clone)]
pub struct OwnedLaunchEnvironment {
    process: Process,
    identity: aterm_uds::process::Identity,
    values: BTreeMap<OsString, OsString>,
}
impl std::fmt::Debug for OwnedLaunchEnvironment {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("OwnedLaunchEnvironment(redacted)")
    }
}
impl OwnedLaunchEnvironment {
    /// Compare captured startup settings for selecting an existing backend.
    /// The native constructor's replaced nonce is excluded. This compares DATA
    /// only; it is neither a current-process check nor SDK/attachment authority.
    pub fn same_settings(&self, other: &Self) -> bool {
        fn settings(
            values: &BTreeMap<OsString, OsString>,
        ) -> impl Iterator<Item = (&OsString, &OsString)> {
            values
                .iter()
                .filter(|(key, _)| key.as_os_str() != std::ffi::OsStr::new(LAUNCH_ENV))
        }
        settings(&self.values).eq(settings(&other.values))
    }
    /// Retain explicitly selected startup DATA for this actual process. The
    /// caller must obtain the complete snapshot on its authenticated control
    /// channel. Neither these bytes nor process metadata grant SDK/provider use.
    ///
    /// # Errors
    /// Invalid/duplicate settings or a changed/unobservable process.
    pub fn from_snapshot(
        process: &Process,
        settings: Vec<(OsString, OsString)>,
    ) -> Result<Self, Error> {
        let pid = i32::try_from(process.pid).map_err(|_| Error::Unavailable)?;
        let identity = aterm_uds::process::read_identity(pid).ok_or(Error::Unavailable)?;
        if Process::observe(process.pid).as_ref() != Some(process) {
            return Err(Error::Stale);
        }
        let mut values = BTreeMap::new();
        for (key, value) in settings {
            if key.is_empty()
                || key.as_bytes().contains(&b'=')
                || key.as_bytes().contains(&0)
                || value.as_bytes().contains(&0)
                || values.insert(key, value).is_some()
            {
                return Err(Error::Denied);
            }
        }
        let result = Self {
            process: process.clone(),
            identity,
            values,
        };
        result.current()?;
        Ok(result)
    }

    /// Prepare the complete UTF-8 admission view before `worker::admit` or its
    /// checked preparation. Non-UTF-8 ordinary settings retain their raw bytes;
    /// file-auth selectors must be representable by the worker admission API.
    /// Explicit starter overrides and removals take precedence over the copy.
    ///
    /// # Errors
    /// Invalid/duplicate overrides, or a non-UTF-8 file-auth selector.
    pub fn prepare(&self, mut starter: worker::Starter) -> Result<worker::Starter, Error> {
        let values = self.effective(&starter)?;
        let mut view = Vec::new();
        for (key, value) in values {
            match (key.to_str(), value.to_str()) {
                (Some(key), Some(value)) => view.push((key.into(), value.into())),
                (Some("HOME" | "CODEX_HOME" | "CODEX_AUTH_PATH" | "CODEX_AUTH_SLOT"), None) => {
                    return Err(Error::Unsupported);
                }
                _ => {}
            }
        }
        starter.env = view;
        Ok(starter)
    }

    pub(super) fn current(&self) -> Result<(), Error> {
        let pid = i32::try_from(self.process.pid).map_err(|_| Error::Unavailable)?;
        if aterm_uds::process::read_identity(pid) != Some(self.identity)
            || Process::observe(self.process.pid).as_ref() != Some(&self.process)
        {
            return Err(Error::Stale);
        }
        Ok(())
    }

    pub(super) fn check(&self, starter: &worker::Starter) -> Result<(), Error> {
        // Every inherited UTF-8 entry must already be visible to admission.
        // Adding the source environment only after the sign-in check is refused.
        let projected = self.prepare(starter.clone())?;
        let declared = declared(starter)?;
        if declared != projected.env.into_iter().collect() {
            return Err(Error::Denied);
        }
        Ok(())
    }

    pub(super) fn admit(self, starter: &worker::Starter) -> Result<AdmittedEnvironment, Error> {
        self.current()?;
        self.check(starter)?;
        let mut selected = self.effective(starter)?;
        selected.remove(std::ffi::OsStr::new(LAUNCH_ENV));
        Ok(AdmittedEnvironment {
            source: self,
            selected,
        })
    }

    fn effective(&self, starter: &worker::Starter) -> Result<BTreeMap<OsString, OsString>, Error> {
        let mut values = self.values.clone();
        values.extend(
            declared(starter)?
                .into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        for key in &starter.unset {
            if key.is_empty() || key.contains(['=', '\0']) {
                return Err(Error::Denied);
            }
            values.remove(std::ffi::OsStr::new(key));
        }
        Ok(values)
    }
}

pub(super) struct AdmittedEnvironment {
    source: OwnedLaunchEnvironment,
    selected: BTreeMap<OsString, OsString>,
}

/// The only executable command produced by complete environment installation.
/// Its command is private so no caller can edit environment settings after this
/// boundary. Constructing it is not SDK readiness or a provider grant.
pub(super) struct InstalledCommand {
    command: aterm_uds::spawnfd::Command,
}

/// Actual native spawn custody from the sealed command, not an arbitrary child
/// or a caller-reported process. Only InstalledCommand can construct this value.
pub(super) struct InstalledSpawn {
    spawned: aterm_uds::spawnfd::Spawned,
}

impl InstalledCommand {
    pub(super) fn spawn(
        mut self,
        attempt: aterm_uds::spawnfd::BirthAttempt,
    ) -> Result<InstalledSpawn, aterm_uds::spawnfd::SpawnFailure> {
        self.command
            .spawn_cohort(attempt)
            .map(|spawned| InstalledSpawn { spawned })
    }
}

impl InstalledSpawn {
    pub(super) fn into_spawned(self) -> aterm_uds::spawnfd::Spawned {
        self.spawned
    }
}

impl AdmittedEnvironment {
    pub(super) fn command(
        &self,
        starter: &worker::Starter,
        sid: &str,
        cwd: &std::path::Path,
        input: aterm_uds::spawnfd::Stdio,
        required_tether: bool,
    ) -> Result<InstalledCommand, Error> {
        let args = starter.args.as_ref().map_err(|_| Error::Unsupported)?;
        let mut command = aterm_uds::spawnfd::command(&starter.exe);
        self.install(&mut command, starter)?;
        command
            .args(args)
            .current_dir(cwd)
            .stdin(input)
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::piped())
            .env("ATERM_SESSION_ID", sid)
            .env("ATERM_PARENT_SESSION_ID", sid);
        if required_tether {
            command.require_tether();
        }
        Ok(InstalledCommand { command })
    }
    pub(super) fn check(&self, starter: &worker::Starter) -> Result<(), Error> {
        self.source.current()?;
        self.source.check(starter)?;
        let mut selected = self.source.effective(starter)?;
        // The actual native arm replaces this one constructor nonce; it does
        // not grant permission to change the admitted execution environment.
        selected.remove(std::ffi::OsStr::new(LAUNCH_ENV));
        if selected != self.selected {
            return Err(Error::Denied);
        }
        Ok(())
    }
    fn install(
        &self,
        command: &mut aterm_uds::spawnfd::Command,
        starter: &worker::Starter,
    ) -> Result<(), Error> {
        self.check(starter)?;
        command.env_clear().envs(self.source.effective(starter)?);
        Ok(())
    }
}

fn declared(starter: &worker::Starter) -> Result<BTreeMap<String, String>, Error> {
    let mut values = BTreeMap::new();
    for (key, value) in &starter.env {
        if key.is_empty()
            || key.contains(['=', '\0'])
            || value.contains('\0')
            || values.insert(key.clone(), value.clone()).is_some()
        {
            return Err(Error::Denied);
        }
    }
    Ok(values)
}

#[cfg(test)]
#[path = "codex_source_owned_environment_tests.rs"]
mod tests;
