// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Owned startup: no postbirth failure becomes an invented no-child outcome.
use super::*;
use aterm_uds::spawnfd::{BirthAttempt, SpawnFailure, Spawned, Stdio};
use std::io;
use std::path::Path;

struct Input {
    stdio: Stdio,
    required_tether: bool,
}

/// A failed native start, retaining the actual attempt outcome and any child.
/// The caller must settle an Unbound cohort; this value is not a cancellation
/// receipt unless its outcome contains the actual NotSpawned receipt.
#[derive(Debug)]
pub struct OwnedSpawnFailure {
    /// Constructor admission or binding refusal, separate from native birth.
    pub reason: Error,
    /// Actual native outcome, including postbirth custody on observation failure.
    pub outcome: SpawnFailure,
}

/// Startup custody of the actual native child and its constructor.
/// Drop kills and reaps this original cohort, so create, supervise and drop it
/// on a worker thread. Transfer its actual receipt explicitly to a running
/// supervisor. This is neither SDK readiness nor closure of separate tool groups.
#[must_use = "retain native child custody until it is settled or transferred"]
pub struct OwnedWorker {
    spawned: Option<Spawned>,
    _constructor: OwnedLaunchReservation,
}

impl OwnedLaunchReservation {
    /// Spawn only this armed admission, then bind its actual native child.
    /// Persist the attempt key before this call when starting a durable job.
    /// This runs off the GUI thread and never enables a task or autopilot.
    ///
    /// # Errors
    /// Admission, scope, source gate or argv validation fails before birth; native
    /// spawn or exec observation fails; or the actual child cannot bind. Every
    /// failure retains its genuine no-child or postbirth outcome for settlement.
    pub fn spawn(
        self,
        admitted: worker::Admitted,
        cwd: &Path,
        attempt: BirthAttempt,
    ) -> Result<OwnedWorker, OwnedSpawnFailure> {
        self.spawn_with_input(
            admitted,
            cwd,
            attempt,
            Input {
                stdio: Stdio::piped(),
                required_tether: false,
            },
        )
    }

    /// Start with a private bidirectional socket at the actual child's stdin.
    /// Native code owns the other endpoint, allowing descriptor delivery without
    /// a public listener or a saved locator. Standard output and error remain
    /// separate pipes. This bootstrap channel grants no SDK or source authority;
    /// SDK construction must still be independently observed before attachment.
    ///
    /// # Errors
    /// Socket creation fails before birth, or any admission, native spawn or
    /// child-binding condition documented by `spawn` fails. Every failure keeps
    /// its genuine no-child or postbirth outcome.
    pub fn spawn_with_control(
        self,
        admitted: worker::Admitted,
        cwd: &Path,
        attempt: BirthAttempt,
    ) -> Result<(OwnedWorker, aterm_uds::CtlStream), OwnedSpawnFailure> {
        self.spawn_control(admitted, cwd, attempt, false)
    }

    /// Backend startup requires a real routed parent-death watcher in addition
    /// to child/socket custody. Unavailable routing refuses before birth; no
    /// untethered fallback or claim about separate tool groups is supplied.
    ///
    /// # Errors
    /// Required parent-death routing is unavailable, or the admitted native
    /// birth/control setup fails. Every failure retains its actual outcome.
    pub fn spawn_with_required_control(
        self,
        admitted: worker::Admitted,
        cwd: &Path,
        attempt: BirthAttempt,
    ) -> Result<(OwnedWorker, aterm_uds::CtlStream), OwnedSpawnFailure> {
        self.spawn_control(admitted, cwd, attempt, true)
    }

    fn spawn_control(
        self,
        admitted: worker::Admitted,
        cwd: &Path,
        attempt: BirthAttempt,
        required_tether: bool,
    ) -> Result<(OwnedWorker, aterm_uds::CtlStream), OwnedSpawnFailure> {
        let (native, child) = match aterm_uds::kept_out(aterm_uds::CtlStream::pair) {
            Ok(pair) => pair,
            Err(_) => return Err(before_birth(Error::Unavailable, attempt)),
        };
        let retained = match aterm_uds::kept_out(|| native.try_clone()) {
            Ok(stream) => stream,
            Err(_) => return Err(before_birth(Error::Unavailable, attempt)),
        };
        let control = match control::Control::new(retained) {
            Ok(control) => control,
            Err(_) => return Err(before_birth(Error::Unavailable, attempt)),
        };
        match lock(&self.binding.1) {
            Ok(mut current) if current.is_none() => *current = Some(Arc::new(control)),
            _ => return Err(before_birth(Error::Stale, attempt)),
        }
        let child: std::os::fd::OwnedFd = child.into();
        self.spawn_with_input(
            admitted,
            cwd,
            attempt,
            Input {
                stdio: child.into(),
                required_tether,
            },
        )
        .map(|worker| (worker, native))
    }

    fn spawn_with_input(
        self,
        admitted: worker::Admitted,
        cwd: &Path,
        attempt: BirthAttempt,
        input: Input,
    ) -> Result<OwnedWorker, OwnedSpawnFailure> {
        let sid = match self.spawn_session() {
            Ok(sid) => sid,
            Err(error) => return Err(before_birth(error, attempt)),
        };
        if let Err(error) = self.check_for_spawn(&admitted) {
            return Err(before_birth(error, attempt));
        }
        let command = match self.environment.command(
            admitted.starter(),
            &sid,
            cwd,
            input.stdio,
            input.required_tether,
        ) {
            Ok(command) => command,
            Err(error) => return Err(before_birth(error, attempt)),
        };
        self.finish_spawn(command.spawn(attempt))
    }

    pub(super) fn finish_spawn(
        self,
        outcome: Result<owned_environment::InstalledSpawn, SpawnFailure>,
    ) -> Result<OwnedWorker, OwnedSpawnFailure> {
        let mut spawned = outcome
            .map_err(|outcome| OwnedSpawnFailure {
                reason: Error::Unavailable,
                outcome,
            })?
            .into_spawned();
        if !spawned.is_original_child() || !spawned.key().is_current() {
            return Err(OwnedSpawnFailure {
                reason: Error::Stale,
                outcome: SpawnFailure::Unbound {
                    spawned: Box::new(spawned),
                    error: io::Error::other("sealed command lost original native spawn custody"),
                },
            });
        }
        if let Err((reason, stage)) = self.bind_checked(spawned.child_mut()) {
            return Err(OwnedSpawnFailure {
                reason,
                outcome: SpawnFailure::Unbound {
                    spawned: Box::new(spawned),
                    error: io::Error::other(format!(
                        "native constructor child bind refused: {stage}"
                    )),
                },
            });
        }
        Ok(OwnedWorker {
            spawned: Some(spawned),
            _constructor: self,
        })
    }
}

pub(super) fn before_birth(reason: Error, attempt: BirthAttempt) -> OwnedSpawnFailure {
    let outcome = match attempt.cancel() {
        Ok(proof) => SpawnFailure::NotSpawned(Box::new(proof)),
        Err(error) => SpawnFailure::Refused(error),
    };
    OwnedSpawnFailure { reason, outcome }
}

impl OwnedWorker {
    /// Transfer this actual child into a native retained service owner. The
    /// control channel comes from its original constructor binding; no caller
    /// supplied stream can substitute for that endpoint. SDK readiness is still
    /// independently required before any frontend delivery.
    ///
    /// # Errors
    /// Missing/stale constructor or control custody. Failure retains the actual
    /// child in `OwnedServiceFailure`, rather than asserting no child exists.
    pub fn into_service(mut self) -> Result<OwnedService, OwnedServiceFailure> {
        let binding = (|| {
            let process = self.process()?;
            self._constructor.binding.entry()?;
            let control = lock(&self._constructor.binding.1)?
                .clone()
                .ok_or(Error::Unsupported)?;
            let state = self
                ._constructor
                .inner
                .state
                .upgrade()
                .ok_or(Error::Stale)?;
            let sid = {
                let state = lock(&state)?;
                let launch = state
                    .launches
                    .get(&self._constructor.inner.token)
                    .ok_or(Error::Stale)?;
                check(launch, &process, &self._constructor.binding)?;
                launch.sid.clone()
            };
            Ok((control, Arc::downgrade(&state), sid))
        })();
        match binding {
            Ok((control, state, sid)) => Ok(service::OwnedService::new(self, control, state, sid)),
            Err(reason) => Err(OwnedServiceFailure {
                reason,
                worker: Box::new(self),
            }),
        }
    }
    /// Borrow the child's startup output. Reading readiness text supplies no SDK grant.
    pub fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.spawned.as_mut()?.child_mut().stderr.take()
    }
    /// Borrow the child's startup output or private protocol pipe.
    pub fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.spawned.as_mut()?.child_mut().stdout.take()
    }
    /// Borrow the actual child's input pipe, never a caller-reported descriptor.
    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.spawned.as_mut()?.child_mut().stdin.take()
    }
    /// The current original child's identity, not signalling or SDK permission.
    ///
    /// # Errors
    /// Custody was transferred or lost, or the kernel cannot observe the child.
    pub fn process(&mut self) -> Result<Process, Error> {
        let spawned = self.spawned.as_mut().ok_or(Error::Stale)?;
        if !spawned.key().is_current()
            || !spawned.is_original_child()
            || !spawned.child_mut().owns_unreaped()
        {
            return Err(Error::Stale);
        }
        Process::observe(spawned.child_mut().id()).ok_or(Error::Unavailable)
    }
    /// Observe terminal exit through this actual child handle without reaping it.
    /// This is diagnostic data at one instant, not SDK readiness or evidence of
    /// which actor caused exit. Original cohort settlement remains unchanged.
    ///
    /// # Errors
    /// Original child custody is unavailable or the kernel exit observation fails.
    #[cfg(debug_assertions)]
    pub fn observe_child_exit_status(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        let spawned = self
            .spawned
            .as_mut()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        if !spawned.key().is_current()
            || !spawned.is_original_child()
            || !spawned.child_mut().owns_unreaped()
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        spawned.child_mut().observe_exit_status()
    }
    /// Observe the physical control-channel binding by this exact unreaped child.
    /// This checks its original challenge and still-issued constructor member on
    /// the live SDK connection, independently of SDK construction readiness.
    /// It grants neither frontend attachment nor source use; recheck it when
    /// committing a delivery alongside the actual SDK and frontend admissions.
    ///
    /// # Errors
    /// The channel is absent, its challenge is pending, the member or connection
    /// ended, or actual child/constructor custody is stale or unavailable.
    pub fn check_control_binding(&mut self) -> Result<(), Error> {
        let process = self.process()?;
        self._constructor.check_control_binding(&process)?;
        if self.process()? != process {
            return Err(Error::Stale);
        }
        Ok(())
    }
    /// Observe completed SDK construction by this exact, still-owned child.
    /// Accept only native accepted manager readiness or an accepted SDK provider
    /// that uses no native reader. Startup text and reader-only registrations do
    /// not qualify. This observation grants no source use or attachment authority;
    /// callers must recheck it when committing attachment.
    ///
    /// # Errors
    /// Custody or the constructor is stale, or SDK construction is still pending
    /// or disconnected. A later SDK reconnect may make a pending check succeed.
    pub fn check_sdk_construction(&mut self) -> Result<(), Error> {
        let process = self.process()?;
        self._constructor.check_sdk_construction(&process)?;
        if self.process()? != process {
            return Err(Error::Stale);
        }
        Ok(())
    }
    /// Transfer the actual cohort receipt to the running supervisor. This neither
    /// acknowledges SDK construction nor declares a backend ready for attachment.
    ///
    /// # Errors
    /// The child was already stopped or its custody was transferred.
    pub fn into_spawned(mut self) -> Result<Spawned, Error> {
        self.spawned.take().ok_or(Error::Stale)
    }
    /// Kill the original cohort before reaping its leader. On error, retain the
    /// actual child for another settlement attempt; do not claim resource closure.
    ///
    /// # Errors
    /// Original custody is lost, the kernel refuses group termination, or wait fails.
    /// WouldBlock retains the actual receipt while members or kernel absence remain
    /// unresolved, including after the original leader's owning reap.
    pub fn stop(&mut self) -> io::Result<()> {
        let Some(spawned) = self.spawned.as_mut() else {
            return Ok(());
        };
        stop_spawned(spawned)?;
        self.spawned.take();
        Ok(())
    }
}

impl OwnedSpawnFailure {
    /// Settle an actual post-birth original cohort without replacing its failure
    /// with a no-child claim. Pending cleanup retains the same owned receipt.
    /// Pre-birth failure has no returned child to stop and remains unchanged.
    ///
    /// # Errors
    /// As `OwnedWorker::stop`; unavailable or pending custody is never closure.
    pub fn stop(&mut self) -> io::Result<()> {
        if let SpawnFailure::Unbound { spawned, .. } = &mut self.outcome {
            stop_spawned(spawned)?;
        }
        Ok(())
    }
}

fn stop_spawned(spawned: &mut Spawned) -> io::Result<()> {
    if !spawned.key().is_current() || !spawned.is_original_child() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "original native child custody is unavailable",
        ));
    }
    let group = spawned.child_mut().id();
    if !spawned.child_mut().is_reaped() {
        if !spawned.child_mut().owns_unreaped() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "original native child wait custody is unavailable",
            ));
        }
        let group = i32::try_from(group).map_err(io::Error::other)?;
        aterm_uds::process::kill_group_until_empty(group)?;
        if !spawned.group_members_ended()? {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "native cohort members have not ended",
            ));
        }
        spawned.child_mut().wait()?;
    }
    // An owning wait only reaps the leader. Retain this actual receipt
    // until the same scope's kernel reports absence of the entire group.
    // After reap, never signal its numeric ID: another birth may reuse it.
    if aterm_uds::process::group_presence(group) != aterm_uds::process::ProcessPresence::Absent {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "native cohort kernel absence remains unresolved",
        ));
    }
    Ok(())
}

impl Drop for OwnedWorker {
    fn drop(&mut self) {
        loop {
            match self.stop() {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    // This owner is created and dropped off the GUI thread.
                    // A pending group cannot surrender its actual custody.
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => {
                    aterm_log::warn!(
                        "owned native constructor cleanup remains unresolved: {error}"
                    );
                    break;
                }
            }
        }
    }
}

impl std::fmt::Debug for OwnedWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnedNativeWorker(redacted)")
    }
}
