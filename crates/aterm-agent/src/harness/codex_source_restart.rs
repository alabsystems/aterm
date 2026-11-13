// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A fresh source constructor for an actual native launcher child. A pending
//! token cannot bind before its exact host operation grants Execute.
use super::*;
use crate::harness::native_act::RestartCommand;

pub(super) struct NativeRestart {
    pub(super) child: Process,
    granted: AtomicBool,
    revoked: AtomicBool,
    #[cfg(target_os = "macos")]
    entry_generation: std::sync::OnceLock<i32>,
}
impl NativeRestart {
    pub(super) fn admits(&self, process: &Process) -> bool {
        #[cfg(target_os = "macos")]
        if self
            .entry_generation
            .get()
            .is_some_and(|bound| kernel_generation(process.pid) != Some(*bound))
        {
            return false;
        }
        !self.revoked.load(Ordering::Acquire)
            && self.granted.load(Ordering::Acquire)
            && self.child.pid == process.pid
            && self.child.birth == process.birth
            && self.child.parent == process.parent
            && self.child.group == process.group
            && self.child.session == process.session
    }
    // Bind once at the real constructor front door, not on a later activation
    // request. A second exec of the same inode must not reuse SDK admission.
    pub(super) fn bind_generation(&self, process: &Process) -> Result<(), Error> {
        if !self.admits(process) {
            return Err(Error::Denied);
        }
        #[cfg(target_os = "macos")]
        {
            let generation = kernel_generation(process.pid).ok_or(Error::Unavailable)?;
            if Process::observe(process.pid).as_ref() != Some(process)
                || kernel_generation(process.pid) != Some(generation)
                || *self.entry_generation.get_or_init(|| generation) != generation
            {
                return Err(Error::Stale);
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = process;
        Ok(())
    }
    pub(super) fn cancelled(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }
}

#[cfg(target_os = "macos")]
fn kernel_generation(pid: u32) -> Option<i32> {
    aterm_uds::process::read_identity(i32::try_from(pid).ok()?)?.idversion
}

pub struct RestartReservation {
    _launch: LaunchReservation,
    native: Arc<NativeRestart>,
    command: RestartCommand,
}
impl std::fmt::Debug for RestartReservation {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("RestartSourceConstructor(<private>)")
    }
}
impl RestartReservation {
    pub fn command(&self) -> &RestartCommand {
        &self.command
    }
    /// The native host calls this only while publishing the exact journaled Go
    /// for this retained child and command. No reader or readiness is fabricated.
    pub fn grant(&self, child: &Process) -> Result<(), Error> {
        if self.native.child != *child || self.native.cancelled() {
            return Err(Error::Stale);
        }
        self.native.granted.store(true, Ordering::Release);
        Ok(())
    }
    pub fn matches(&self, command: &RestartCommand, child: &Process) -> bool {
        if self.native.child != *child {
            return false;
        }
        let mut original = self.command.clone();
        original.env.retain(|(name, _)| name != LAUNCH_ENV);
        original == *command
    }
}
impl Drop for RestartReservation {
    fn drop(&mut self) {
        // Synchronous revocation needs no registry/native/account lock. The
        // registry sweep disposes the ungranted gate outside its State guard.
        if !self.native.granted.load(Ordering::Acquire) {
            self.native.revoked.store(true, Ordering::Release);
        }
    }
}

/// A fresh constructor reserved for the existing SDK owner itself. The native
/// host still owns signed-image admission, policy, input custody and its Go
/// record; this reservation cannot grant entry while old SDK custody remains.
pub struct SourceExecReservation {
    restart: RestartReservation,
    caller: slot::Caller,
}

impl std::fmt::Debug for SourceExecReservation {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("SourceExecConstructor(<private>)")
    }
}

impl SourceExecReservation {
    pub fn command(&self) -> &RestartCommand {
        self.restart.command()
    }

    /// Observe entry through this exact granted constructor. A matching image
    /// or PID alone does not establish SDK admission. This publishes no grant.
    pub fn entered(&self, process: &Process) -> Result<bool, Error> {
        if process == &self.restart.native.child
            || process.image != self.restart.command.image
            || !self.restart.native.admits(process)
            || Process::observe(process.pid).as_ref() != Some(process)
        {
            return Ok(false);
        }
        let state = self.restart._launch.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        Ok(state
            .actors
            .get(&actor_key(&self.caller))
            .is_some_and(|actor| actor.process == *process)
            && state
                .launches
                .get(&self.restart._launch.token)
                .is_some_and(|launch| {
                    launch.process.as_ref() == Some(process)
                        && launch
                            .native_restart
                            .as_ref()
                            .is_some_and(|native| Arc::ptr_eq(native, &self.restart.native))
                }))
    }

    /// Called by the native operation owner only when publishing its exact Go.
    /// Check the original kernel image and explicit SDK retirement together with
    /// the registry-owned constructor. EOF and a caller's empty inventory do not
    /// establish retirement. A successful retry retains the same constructor.
    /// An error never retracts an earlier Go or proves rollback is safe.
    pub fn grant(&self) -> Result<(), Error> {
        let original = &self.restart.native.child;
        if Process::observe(original.pid).as_ref() != Some(original) {
            return Err(Error::Stale);
        }
        let state = self.restart._launch.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        if state
            .actors
            .get(&actor_key(&self.caller))
            .is_none_or(|actor| actor.process != *original)
        {
            return Err(Error::Stale);
        }
        retired_actor(&state, &self.caller)?;
        if state
            .launches
            .get(&self.restart._launch.token)
            .and_then(|launch| launch.native_restart.as_ref())
            .is_none_or(|native| !Arc::ptr_eq(native, &self.restart.native))
        {
            return Err(Error::Stale);
        }
        self.restart.grant(original)
    }
}

fn retired_actor(state: &State, caller: &slot::Caller) -> Result<(), Error> {
    let key = actor_key(caller);
    let actor = state.actors.get(&key).ok_or(Error::Stale)?;
    let prefix = format!("{key}:");
    if !actor.issued.is_empty()
        || state
            .plain_retiring
            .keys()
            .any(|member| member.starts_with(&prefix))
        || state
            .members
            .keys()
            .any(|member| member.starts_with(&prefix))
        || state
            .unrelated
            .keys()
            .any(|member| member.starts_with(&prefix))
        || state
            .preloads
            .iter()
            .any(|(member, pending)| member.starts_with(&prefix) && !pending.retired())
        || state.uses.keys().any(|member| {
            member.starts_with(&prefix) && reservation::has_credential_uses(state, member)
        })
        || state.reservations.contains_key(&key)
        || state.retired_reservations.iter().any(|(history, cell)| {
            history.starts_with(&prefix) && lock(cell).map_or(true, |cell| !cell.custody_retired())
        })
        || reservation::target::resources::custody_pending(state, caller)
    {
        return Err(Error::Busy);
    }
    Ok(())
}

/// A granted constructor may enter another image in the same kernel process.
/// Preserve the old actor until explicit member retirement has completed; an
/// image change, socket EOF or matching PID alone cannot erase its SDK custody.
pub(super) fn advance_exec_actor(state: &mut State, peer: &Peer) -> Result<(), Error> {
    let key = actor_key(&peer.caller);
    let Some(actor) = state.actors.get(&key) else {
        return Ok(());
    };
    if actor.process == peer.process {
        return Ok(());
    }
    let launch = peer
        .launch
        .as_ref()
        .and_then(|token| state.launches.get(token))
        .ok_or(Error::Denied)?;
    let native = launch.native_restart.as_ref().ok_or(Error::Denied)?;
    if native.child != actor.process
        || !native.admits(&peer.process)
        || launch.image != peer.process.image
        || launch.process.as_ref() != Some(&peer.process)
        || launch.sid != peer.caller.sid
    {
        return Err(Error::Denied);
    }
    retired_actor(state, &peer.caller)?;
    // New member IDs cannot collide with the old image's retired ordinals or
    // release-only replay metadata, which remains under its original epoch.
    let epoch = aterm_uds::rand::hex_token::<8>().map_err(|_| Error::Unavailable)?;
    version(state)?;
    let actor = state.actors.get_mut(&key).ok_or(Error::Stale)?;
    actor.process = peer.process.clone();
    actor.epoch = epoch;
    actor.highwater = 0;
    Ok(())
}

impl Registry {
    /// Reserve fresh SDK construction for an actual registered source process.
    /// Preparation may precede reader teardown; Go independently requires the
    /// explicit retirement of that exact actor. Legacy file readers are refused.
    pub fn reserve_source_exec(
        &self,
        env: &Env,
        sid: &str,
        command: &RestartCommand,
        source: &Process,
    ) -> Result<SourceExecReservation, Error> {
        let caller = slot::Caller {
            sid: sid.into(),
            pid: source.pid,
            birth: source.birth,
        };
        {
            let state = lock(&self.state)?;
            if state
                .actors
                .get(&actor_key(&caller))
                .is_none_or(|actor| actor.process != *source)
            {
                return Err(Error::Denied);
            }
        }
        let restart = self
            .reserve_native_restart(env, sid, command, source)?
            .ok_or(Error::Unsupported)?;
        Ok(SourceExecReservation { restart, caller })
    }

    /// Called off the native operation locks after actual launcher observation.
    /// This prepares constructor custody, not a native execution grant.
    pub fn reserve_native_restart(
        &self,
        env: &Env,
        sid: &str,
        command: &RestartCommand,
        child: &Process,
    ) -> Result<Option<RestartReservation>, Error> {
        use crate::harness::codex_app_login::legacy_file_reader_allowed;
        if Process::observe(child.pid).as_ref() != Some(child)
            || !sid.starts_with("s-")
            || sid.len() > 64
            || command.env.iter().any(|(name, _)| name == LAUNCH_ENV)
            || command
                .unset
                .as_ref()
                .is_some_and(|names| names.iter().any(|name| name == LAUNCH_ENV))
        {
            return Err(Error::Denied);
        }
        let executable = std::path::PathBuf::from(&command.program);
        if Image::at(&executable).as_ref() != Some(&command.image) {
            return Err(Error::Stale);
        }
        let relevant = ["CODEX_HOME", "CODEX_AUTH_PATH", "CODEX_AUTH_SLOT"];
        let starter = worker::Starter {
            exe: executable.clone(),
            env: command.env.clone(),
            args: Ok(command.argv.clone()),
            unset: relevant
                .iter()
                .copied()
                .filter(|name| {
                    command
                        .unset
                        .as_ref()
                        .is_some_and(|names| names.iter().any(|unset| unset == name))
                })
                .collect(),
        };
        let cwd = match command.cwd.as_deref() {
            Some(cwd) => std::path::PathBuf::from(cwd),
            None => aterm_sysprobe::process_cwd(child.pid).ok_or(Error::Unavailable)?,
        };
        let auth = worker::declared_codex_auth_file(&starter, env.home.as_deref(), &cwd)
            .ok_or(Error::Unavailable)?;
        if !crate::harness::codex_alab::managed_credential_capable(&executable, true)
            .map_err(|_| Error::Unavailable)?
        {
            return if legacy_file_reader_allowed(&auth) {
                Ok(None)
            } else {
                Err(Error::Unsupported)
            };
        }
        if !crate::harness::codex_alab::alab_bytes(&executable) {
            return Err(Error::Unsupported);
        }
        let scope = {
            let state = lock(&self.state)?;
            state.app_lifetime.clone().ok_or(Error::Denied)?
        }
        .enter()
        .ok_or(Error::Denied)?;
        let gate = SourceWorkLease::for_home(env.home.as_deref().ok_or(Error::Unavailable)?)
            .map_err(|_| Error::Busy)?;
        let choice = owner::capture_canonical_launch_choice(env, &auth, &gate)
            .map_err(|_| Error::Unavailable)?;
        let initial = choice.map(|choice| {
            Arc::new(InitialCell {
                choice,
                alive: AtomicBool::new(true),
                installing: AtomicBool::new(false),
                published: AtomicBool::new(false),
            })
        });
        let native = Arc::new(NativeRestart {
            child: child.clone(),
            granted: AtomicBool::new(false),
            revoked: AtomicBool::new(false),
            #[cfg(target_os = "macos")]
            entry_generation: std::sync::OnceLock::new(),
        });
        let token = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let mut state = lock(&self.state)?;
        if scope.cancelled() || state.launches.len() >= MAX_LAUNCHES {
            return Err(Error::Busy);
        }
        state.launches.insert(
            token.clone(),
            Launch {
                sid: sid.into(),
                image: command.image.clone(),
                expected_env: command
                    .env
                    .iter()
                    .filter(|(name, _)| relevant.contains(&name.as_str()))
                    .cloned()
                    .collect(),
                expected_unset: starter.unset,
                process: None,
                typed: true,
                gate: Some(Arc::new(gate)),
                _scope: Some(scope),
                initial,
                native_restart: Some(native.clone()),
            },
        );
        drop(state);
        let mut command = command.clone();
        command.env.push((LAUNCH_ENV.into(), token.clone()));
        Ok(Some(RestartReservation {
            _launch: LaunchReservation {
                token,
                executable,
                state: Arc::downgrade(&self.state),
            },
            native,
            command,
        }))
    }
}

#[cfg(all(test, unix))]
#[path = "codex_source_restart_tests.rs"]
mod tests;
