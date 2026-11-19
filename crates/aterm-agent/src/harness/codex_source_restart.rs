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

/// A UI-only exec constructor in the original launch table. Its nonce may
/// select independent controller entry, but can never open a credential member.
/// The Native owner independently retains and validates its admitted image.
#[cfg(unix)]
pub struct FrontendExecReservation {
    launch: LaunchReservation,
    native: Arc<NativeRestart>,
    source: Process,
    source_identity: aterm_uds::process::Identity,
    target: Image,
}
#[cfg(unix)]
impl FrontendExecReservation {
    /// Correlation DATA for the exact raw command's environment, not a grant.
    pub fn token_data(&self) -> &str {
        self.launch.token()
    }
    /// Called by the original Native owner after H1/capture/image validation
    /// and durable possible-execution recording. A retry grants the same token.
    ///
    /// # Errors
    /// The original kernel generation, frontend or retired Source custody changed.
    pub fn grant(
        &self,
        frontend: &ServiceFrontend,
        foreground: &FrontendMember<'_>,
    ) -> Result<(), Error> {
        frontend.check_foreground(foreground)?;
        if frontend.process() != &self.source
            || aterm_uds::process::read_identity(self.source_identity.pid)
                != Some(self.source_identity)
            || Process::observe(self.source.pid).as_ref() != Some(&self.source)
        {
            return Err(Error::Stale);
        }
        let state = self.launch.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        if !frontend.in_namespace(&self.launch.state) {
            return Err(Error::Denied);
        }
        if state.actors.contains_key(&actor_key(foreground.caller())) {
            retired_actor(&state, foreground.caller())?;
        }
        let launch = state
            .launches
            .get(self.launch.token())
            .ok_or(Error::Stale)?;
        if !matches!(&launch.binding, LaunchBinding::Frontend)
            || launch
                .native_restart
                .as_ref()
                .is_none_or(|native| !Arc::ptr_eq(native, &self.native))
            || launch.image != self.target
        {
            return Err(Error::Stale);
        }
        self.native.grant_frontend(&self.source)
    }
    /// Verify actual entry through this same token and a different exec image
    /// generation before any E1 bytes are released. No actor/Ready is required.
    ///
    /// # Errors
    /// The actual issued frontend/foreground/constructor or generation is foreign/stale.
    pub fn entered(
        &self,
        frontend: &ServiceFrontend,
        foreground: &FrontendMember<'_>,
    ) -> Result<(), Error> {
        frontend.check_foreground(foreground)?;
        let process = frontend.process();
        if process.image != self.target
            || !self.native.admits(process)
            || !frontend.in_namespace(&self.launch.state)
        {
            return Err(Error::Denied);
        }
        let identity = aterm_uds::process::read_identity(
            i32::try_from(process.pid).map_err(|_| Error::Unavailable)?,
        )
        .ok_or(Error::Unavailable)?;
        if (identity.idversion.is_some() && identity == self.source_identity)
            || Process::observe(process.pid).as_ref() != Some(process)
        {
            return Err(Error::Stale);
        }
        let state = self.launch.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        let launch = state
            .launches
            .get(self.launch.token())
            .ok_or(Error::Stale)?;
        if !matches!(&launch.binding, LaunchBinding::Frontend)
            || launch.process.as_ref() != Some(process)
            || launch
                .native_restart
                .as_ref()
                .is_none_or(|native| !Arc::ptr_eq(native, &self.native))
        {
            return Err(Error::Denied);
        }
        self.native.bind_generation(process)
    }
}
#[cfg(unix)]
impl NativeRestart {
    fn grant_frontend(&self, original: &Process) -> Result<(), Error> {
        if self.child != *original || self.cancelled() {
            return Err(Error::Stale);
        }
        self.granted.store(true, Ordering::Release);
        Ok(())
    }
}
#[cfg(unix)]
impl Drop for FrontendExecReservation {
    fn drop(&mut self) {
        if !self.native.granted.load(Ordering::Acquire) {
            self.native.revoked.store(true, Ordering::Release);
        }
    }
}

#[cfg(unix)]
impl Registry {
    /// Reserve only UI/controller entry for the actual current foreground.
    /// The Native caller must retain its independently admitted target image;
    /// raw argv/environment remain with that owner, never shell-reparsed here.
    ///
    /// # Errors
    /// Foreign/ended frontend, a live old credential reader, invalid/raw auth
    /// selectors, changed image/process, closed App, or exhausted launch capacity.
    pub fn reserve_frontend_exec(
        &self,
        frontend: &ServiceFrontend,
        foreground: &FrontendMember<'_>,
        target: &Image,
        environment: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Result<FrontendExecReservation, Error> {
        use std::os::unix::ffi::OsStrExt;
        frontend.check_foreground(foreground)?;
        let namespace = Arc::downgrade(&self.state);
        if !frontend.in_namespace(&namespace) {
            return Err(Error::Denied);
        }
        let source = frontend.process().clone();
        let source_identity = aterm_uds::process::read_identity(
            i32::try_from(source.pid).map_err(|_| Error::Unavailable)?,
        )
        .ok_or(Error::Unavailable)?;
        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(&target.path));
        if source.image == *target || Image::at(path).as_ref() != Some(target) {
            return Err(Error::Stale);
        }
        let mut names = std::collections::BTreeSet::new();
        let mut expected_env = Vec::new();
        let mut session = None;
        for (name, value) in environment {
            if name.is_empty()
                || name.as_bytes().contains(&b'=')
                || name.as_bytes().contains(&0)
                || value.as_bytes().contains(&0)
                || !names.insert(name)
            {
                return Err(Error::Denied);
            }
            if name == "ATERM_SESSION_ID" {
                session = value.to_str();
            }
            if matches!(
                name.to_str(),
                Some("CODEX_HOME" | "CODEX_AUTH_PATH" | "CODEX_AUTH_SLOT")
            ) {
                expected_env.push((
                    name.to_str().ok_or(Error::Unsupported)?.into(),
                    value.to_str().ok_or(Error::Unsupported)?.into(),
                ));
            }
        }
        if session != Some(foreground.caller().sid.as_str()) {
            return Err(Error::Denied);
        }
        let token = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let current_token = atpkg::caller_shell::process_args(source.pid)
            .and_then(|args| args.env_var(LAUNCH_ENV).map(str::to_owned));
        let mut state = lock(&self.state)?;
        if state.actors.contains_key(&actor_key(foreground.caller())) {
            retired_actor(&state, foreground.caller())?;
        }
        let scope = state
            .app_lifetime
            .as_ref()
            .ok_or(Error::Denied)?
            .enter()
            .ok_or(Error::Denied)?;
        // An already entered current UI constructor positively supersedes its
        // older generations in this SAME launch table. Do not accumulate one
        // permanent row per successful same-birth upgrade. Ungranted, uncertain
        // and unrelated constructors are never evicted for capacity.
        let current_is_frontend = current_token.as_ref().is_some_and(|token| {
            state.launches.get(token).is_some_and(|launch| {
                matches!(launch.binding, LaunchBinding::Frontend)
                    && launch.process.as_ref() == Some(&source)
                    && launch
                        .native_restart
                        .as_ref()
                        .is_some_and(|native| native.admits(&source))
            })
        });
        let old_tokens: Vec<_> = if current_is_frontend {
            state
                .launches
                .iter()
                .filter(|(key, launch)| {
                    current_token.as_ref() != Some(*key)
                        && matches!(launch.binding, LaunchBinding::Frontend)
                        && launch.process.as_ref().is_some_and(|old| {
                            old.pid == source.pid
                                && old.birth == source.birth
                                && old.image != source.image
                        })
                        && launch
                            .native_restart
                            .as_ref()
                            .is_some_and(|native| native.granted.load(Ordering::Acquire))
                })
                .map(|(key, _)| key.clone())
                .collect()
        } else {
            Vec::new()
        };
        let mut retired = Vec::new();
        for key in old_tokens {
            if let Some(old) = state.launches.remove(&key) {
                if let Some(native) = &old.native_restart {
                    native.revoked.store(true, Ordering::Release);
                }
                retired.push(old);
            }
        }
        if state.launches.len() >= MAX_LAUNCHES || state.launches.contains_key(&token) {
            drop(state);
            drop(retired);
            return Err(Error::Busy);
        }
        let native = Arc::new(NativeRestart {
            child: source.clone(),
            granted: AtomicBool::new(false),
            revoked: AtomicBool::new(false),
            #[cfg(target_os = "macos")]
            entry_generation: std::sync::OnceLock::new(),
        });
        state.launches.insert(
            token.clone(),
            Launch {
                sid: foreground.caller().sid.clone(),
                image: target.clone(),
                expected_env,
                expected_unset: ["CODEX_HOME", "CODEX_AUTH_PATH", "CODEX_AUTH_SLOT"]
                    .into_iter()
                    .filter(|name| {
                        !names
                            .iter()
                            .any(|actual| *actual == std::ffi::OsStr::new(name))
                    })
                    .collect(),
                process: None,
                binding: LaunchBinding::Frontend,
                typed: true,
                gate: None,
                _scope: Some(scope),
                initial: None,
                native_restart: Some(native.clone()),
            },
        );
        drop(state);
        drop(retired);
        Ok(FrontendExecReservation {
            launch: LaunchReservation {
                token,
                executable: path.to_path_buf(),
                state: namespace,
            },
            native,
            source,
            source_identity,
            target: target.clone(),
        })
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
            || aterm_types::control_request::Sid::parse(sid).is_none()
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
                binding: LaunchBinding::Consumer,
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
