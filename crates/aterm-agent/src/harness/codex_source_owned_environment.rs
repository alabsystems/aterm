// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Initial exec environment DATA from an observed process, never SDK admission.
//! Native owned children must not silently inherit the GUI's environment.
use super::*;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

/// A private copy of one observed process's initial exec environment. Debug
/// redacts its values and no serialization is implemented. It is not a process lease or a
/// credential/provider grant; the child still needs its independent constructor.
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
    /// Copy kernel-observed initial environment bytes, bracketed by birth,
    /// mapped image and exec-generation observations where the OS supplies one.
    /// Linux has no exec generation: a same-image exec between observations is
    /// not excluded. This observation is DATA, not physical custody or authority.
    ///
    /// # Errors
    /// The process changed, its environment is unreadable/withheld/empty, or has
    /// ambiguous/malformed entries. An empty answer cannot prove an empty source.
    pub fn capture(process: &Process) -> Result<Self, Error> {
        let pid = i32::try_from(process.pid).map_err(|_| Error::Unavailable)?;
        let identity = aterm_uds::process::read_identity(pid).ok_or(Error::Unavailable)?;
        if Process::observe(process.pid).as_ref() != Some(process) {
            return Err(Error::Stale);
        }
        let values = read(process.pid)?;
        if values.is_empty() {
            return Err(Error::Unavailable);
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
impl AdmittedEnvironment {
    pub(super) fn check_child(
        &self,
        process: &Process,
        token: &str,
        sid: &str,
    ) -> Result<(), Error> {
        let observed = OwnedLaunchEnvironment::capture(process)?;
        let mut expected = self.selected.clone();
        expected.insert(LAUNCH_ENV.into(), token.into());
        expected.insert("ATERM_SESSION_ID".into(), sid.into());
        expected.insert("ATERM_PARENT_SESSION_ID".into(), sid.into());
        if observed.values != expected {
            return Err(Error::Denied);
        }
        Ok(())
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
    pub(super) fn install(
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

fn entries<'a>(
    words: impl Iterator<Item = &'a [u8]>,
) -> Result<BTreeMap<OsString, OsString>, Error> {
    let mut values = BTreeMap::new();
    for word in words {
        let split = word
            .iter()
            .position(|byte| *byte == b'=')
            .ok_or(Error::Unavailable)?;
        if split == 0 || word.contains(&0) {
            return Err(Error::Unavailable);
        }
        let key = OsString::from_vec(word[..split].to_vec());
        let value = OsString::from_vec(word[split + 1..].to_vec());
        if values.insert(key, value).is_some() {
            return Err(Error::Unavailable);
        }
    }
    Ok(values)
}

#[cfg(target_os = "macos")]
fn read(pid: u32) -> Result<BTreeMap<OsString, OsString>, Error> {
    let bytes = aterm_uds::procargs::read(pid).ok_or(Error::Unavailable)?;
    let parsed = aterm_uds::procargs::parse(&bytes)
        .filter(|parsed| parsed.whole())
        .ok_or(Error::Unavailable)?;
    entries(parsed.env())
}
#[cfg(any(target_os = "linux", target_os = "android"))]
fn read(pid: u32) -> Result<BTreeMap<OsString, OsString>, Error> {
    let bytes = aterm_uds::process::read_environ(pid).ok_or(Error::Unavailable)?;
    if !bytes.is_empty() && bytes.last() != Some(&0) {
        return Err(Error::Unavailable);
    }
    entries(
        bytes
            .split(|byte| *byte == 0)
            .filter(|word| !word.is_empty()),
    )
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn read(_pid: u32) -> Result<BTreeMap<OsString, OsString>, Error> {
    Err(Error::Unsupported)
}

#[cfg(test)]
#[path = "codex_source_owned_environment_tests.rs"]
mod tests;
