// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Constructor permission for a child whose actual spawn is owned by native code.
//! This is distinct from a worker typed into a terminal and claiming its launch.
use super::*;
use std::sync::atomic::AtomicU8;

#[path = "codex_source_owned_spawn.rs"]
mod spawn;
pub use spawn::{OwnedSpawnFailure, OwnedWorker};

#[path = "codex_source_owned_control.rs"]
mod control;

const PENDING: u8 = 0;
const BOUND: u8 = 1;
const REVOKED: u8 = 2;

#[derive(Default)]
pub(super) struct Binding(AtomicU8, Mutex<Option<Arc<control::Control>>>);
impl Binding {
    pub(super) fn entry(&self) -> Result<(), Error> {
        match self.0.load(Ordering::Acquire) {
            BOUND => Ok(()),
            PENDING => Err(Error::Busy),
            _ => Err(Error::Denied),
        }
    }
    pub(super) fn revoked(&self) -> bool {
        self.0.load(Ordering::Acquire) == REVOKED
    }
    fn bind(&self) -> Result<(), Error> {
        match self
            .0
            .compare_exchange(PENDING, BOUND, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) | Err(BOUND) => Ok(()),
            _ => Err(Error::Stale),
        }
    }
    fn revoke(&self) {
        let _ = self
            .0
            .compare_exchange(PENDING, REVOKED, Ordering::AcqRel, Ordering::Acquire);
    }
}

/// An independent constructor that cannot be claimed before native child bind.
/// Its launcher must retain and supervise the child; this is not a process owner,
/// an inherited SDK member, or a provider/assignment grant.
pub struct OwnedLaunchReservation {
    inner: LaunchReservation,
    binding: Arc<Binding>,
    environment: owned_environment::AdmittedEnvironment,
}

impl OwnedLaunchReservation {
    pub(super) fn new(
        inner: LaunchReservation,
        binding: Arc<Binding>,
        environment: owned_environment::AdmittedEnvironment,
    ) -> Self {
        Self {
            inner,
            binding,
            environment,
        }
    }

    /// Arm only this independently admitted image, retaining its sign-in check.
    ///
    /// # Errors
    /// The admission, image or source gate changed, or the constructor was bound.
    pub fn arm(
        &self,
        admitted: worker::Admitted,
        cli: &worker::WorkerCli,
        home: Option<&std::path::Path>,
    ) -> Result<worker::Admitted, Error> {
        self.inner.arm(admitted, cli, home)
    }

    /// Recheck the armed admission immediately before the owned spawn.
    ///
    /// # Errors
    /// The image, constructor environment or source gate changed, or it was bound.
    pub fn check_for_spawn(&self, admitted: &worker::Admitted) -> Result<(), Error> {
        self.environment.check(admitted.starter())?;
        self.spawn_session()?;
        self.inner.check_for_write(admitted)
    }

    fn spawn_session(&self) -> Result<String, Error> {
        let state = self.inner.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        let launch = state.launches.get(&self.inner.token).ok_or(Error::Stale)?;
        if !matches!(&launch.binding, LaunchBinding::Native(actual) if Arc::ptr_eq(actual, &self.binding))
            || self.binding.0.load(Ordering::Acquire) != PENDING
            || launch.process.is_some()
            || !launch
                ._scope
                .as_ref()
                .is_some_and(|scope| !scope.cancelled())
        {
            return Err(Error::Stale);
        }
        Ok(launch.sid.clone())
    }

    /// Bind the launcher’s unreaped native child in its own process group.
    /// Kernel birth, parent, session, group, image and launch environment are
    /// observed here; a reported PID or a consumer's hello cannot perform bind.
    /// A repeated bind accepts only that same still-owned child incarnation.
    ///
    /// # Errors
    /// The child is reaped, foreign, not group-leading, changed during observation,
    /// or does not match this live admitted constructor and source gate.
    pub fn bind(&self, child: &aterm_uds::spawnfd::Child) -> Result<(), Error> {
        if !child.owns_unreaped() {
            return Err(Error::Stale);
        }
        let process = Process::observe(child.id()).ok_or(Error::Unavailable)?;
        let parent = Process::observe(std::process::id()).ok_or(Error::Unavailable)?;
        if process.parent != parent.pid
            || process.group != process.pid
            || process.session != parent.session
        {
            return Err(Error::Denied);
        }
        let args = atpkg::caller_shell::process_args(process.pid).ok_or(Error::Unavailable)?;
        if args.env_var(LAUNCH_ENV) != Some(self.inner.token.as_str()) {
            return Err(Error::Denied);
        }
        let state = self.inner.state.upgrade().ok_or(Error::Stale)?;
        let (gate, sid) = {
            let state = lock(&state)?;
            let launch = state.launches.get(&self.inner.token).ok_or(Error::Stale)?;
            check(launch, &process, &self.binding)?;
            if launch
                .expected_env
                .iter()
                .any(|(key, value)| args.env_var(key) != Some(value.as_str()))
                || launch
                    .expected_unset
                    .iter()
                    .any(|name| args.env_var(name).is_some())
            {
                return Err(Error::Denied);
            }
            (
                launch.gate.as_ref().ok_or(Error::Stale)?.clone(),
                launch.sid.clone(),
            )
        };
        // Profile and process observation never run under the registry mutex.
        self.environment
            .check_child(&process, &self.inner.token, &sid)?;
        gate.current().map_err(|_| Error::Stale)?;
        if !child.owns_unreaped() || Process::observe(child.id()).as_ref() != Some(&process) {
            return Err(Error::Stale);
        }
        let mut state = lock(&state)?;
        let launch = state
            .launches
            .get_mut(&self.inner.token)
            .ok_or(Error::Stale)?;
        check(launch, &process, &self.binding)?;
        if !launch
            .gate
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &gate))
        {
            return Err(Error::Stale);
        }
        self.binding.bind()?;
        launch.bind(process)?;
        Ok(())
    }

    /// Revoke only an unclaimed constructor; never assert that a child exited.
    ///
    /// # Errors
    /// The reservation is stale, contended, or its actual child already bound.
    pub fn revoke_unclaimed(self) -> Result<(), Error> {
        self.binding.revoke();
        self.inner.revoke_unclaimed_inner()
    }

    pub(super) fn check_control_binding(&self, process: &Process) -> Result<(), Error> {
        self.binding.entry()?;
        let control = lock(&self.binding.1)?
            .as_ref()
            .cloned()
            .ok_or(Error::Unsupported)?;
        let (connection, member) = control.bound()?;
        let state = self.inner.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        let launch = state.launches.get(&self.inner.token).ok_or(Error::Stale)?;
        check(launch, process, &self.binding)?;
        if launch.process.as_ref() != Some(process) || !connection.is_live() {
            return Err(Error::Stale);
        }
        let caller = slot::Caller {
            sid: launch.sid.clone(),
            pid: process.pid,
            birth: process.birth,
        };
        let actor = state.actors.get(&actor_key(&caller)).ok_or(Error::Stale)?;
        let issued = actor.issued.get(&member).ok_or(Error::Stale)?;
        if actor.process != *process
            || issued.target.is_some()
            || issued.parent_use.is_some()
            || !issued.connection.same(&connection)
        {
            return Err(Error::Stale);
        }
        Ok(())
    }

    pub(super) fn check_sdk_construction(&self, process: &Process) -> Result<(), Error> {
        self.binding.entry()?;
        let state = self.inner.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        let launch = state.launches.get(&self.inner.token).ok_or(Error::Stale)?;
        check(launch, process, &self.binding)?;
        if launch.process.as_ref() != Some(process) {
            return Err(Error::Stale);
        }
        let actor_key = format!("{}:{}:{}", launch.sid, process.pid, process.birth);
        let actor = state.actors.get(&actor_key).ok_or(Error::Busy)?;
        if actor.process != *process {
            return Err(Error::Stale);
        }
        for (member, issued) in &actor.issued {
            if issued.target.is_some() || !issued.connection.is_live() {
                continue;
            }
            let key = format!("{actor_key}:{member}");
            if let Some(current) = state.members.get(&key)
                && current.process == *process
                && current.operation == issued.operation
                && current.connection.same(&issued.connection)
                && !current.unknown
                && current.readiness.manager()
                && !state.preloads.contains_key(&key)
                && !reservation::target::normalize::handoff::source_revoked(
                    &state,
                    &current.caller,
                    member,
                )
            {
                return Ok(());
            }
            if let Some(current) = state.unrelated.get(&key)
                && current.process == *process
                && current.operation == issued.operation
                && current.connection.same(&issued.connection)
            {
                return Ok(());
            }
        }
        Err(Error::Busy)
    }
}

fn check(launch: &Launch, process: &Process, binding: &Arc<Binding>) -> Result<(), Error> {
    if !matches!(&launch.binding, LaunchBinding::Native(actual) if Arc::ptr_eq(actual, binding))
        || binding.revoked()
        || launch.native_restart.is_some()
        || launch.image != process.image
        || launch
            .process
            .as_ref()
            .is_some_and(|bound| bound != process)
        || launch
            ._scope
            .as_ref()
            .is_some_and(|scope| scope.cancelled())
    {
        return Err(Error::Denied);
    }
    Ok(())
}

impl Drop for OwnedLaunchReservation {
    fn drop(&mut self) {
        // Revocation takes no registry, account or native lock. Sweeps dispose
        // an unclaimed constructor; a bound child retains its real SDK lifetime.
        self.binding.revoke();
    }
}

impl std::fmt::Debug for OwnedLaunchReservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnedNativeConstructor(redacted)")
    }
}

#[cfg(test)]
#[path = "codex_source_owned_launch_tests.rs"]
mod tests;
