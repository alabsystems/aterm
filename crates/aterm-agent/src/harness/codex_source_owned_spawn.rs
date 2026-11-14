// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Owned startup: no postbirth failure becomes an invented no-child outcome.
use super::*;
use aterm_uds::spawnfd::{BirthAttempt, SpawnFailure, Spawned, Stdio};
use std::io;
use std::path::Path;

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
        cli: &worker::WorkerCli,
        home: Option<&Path>,
        cwd: &Path,
        attempt: BirthAttempt,
    ) -> Result<OwnedWorker, OwnedSpawnFailure> {
        self.spawn_with_input(admitted, cli, home, cwd, attempt, Stdio::piped())
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
        cli: &worker::WorkerCli,
        home: Option<&Path>,
        cwd: &Path,
        attempt: BirthAttempt,
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
        self.spawn_with_input(admitted, cli, home, cwd, attempt, child.into())
            .map(|worker| (worker, native))
    }

    fn spawn_with_input(
        self,
        admitted: worker::Admitted,
        cli: &worker::WorkerCli,
        home: Option<&Path>,
        cwd: &Path,
        attempt: BirthAttempt,
        input: Stdio,
    ) -> Result<OwnedWorker, OwnedSpawnFailure> {
        let admitted = match admitted.prepare(cli, home, |starter| Ok((starter, ()))) {
            Ok((admitted, ())) => admitted,
            Err(_) => return Err(before_birth(Error::Denied, attempt)),
        };
        let sid = match self.spawn_session() {
            Ok(sid) => sid,
            Err(error) => return Err(before_birth(error, attempt)),
        };
        if let Err(error) = self.check_for_spawn(&admitted) {
            return Err(before_birth(error, attempt));
        }
        let starter = admitted.starter();
        let args = match &starter.args {
            Ok(args) => args,
            Err(_) => return Err(before_birth(Error::Unsupported, attempt)),
        };
        let mut command = aterm_uds::spawnfd::command(&starter.exe);
        if let Err(error) = self.environment.install(&mut command, starter) {
            return Err(before_birth(error, attempt));
        }
        command
            .args(args)
            .current_dir(cwd)
            .stdin(input)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in &starter.unset {
            command.env_remove(name);
        }
        // Both native peer observation and the SDK locator must name the actual
        // owning terminal, rather than the GUI process's unrelated environment.
        command
            .env("ATERM_SESSION_ID", &sid)
            .env("ATERM_PARENT_SESSION_ID", &sid);
        self.finish_spawn(command.spawn_cohort(attempt))
    }

    pub(super) fn finish_spawn(
        self,
        outcome: Result<Spawned, SpawnFailure>,
    ) -> Result<OwnedWorker, OwnedSpawnFailure> {
        let mut spawned = outcome.map_err(|outcome| OwnedSpawnFailure {
            reason: Error::Unavailable,
            outcome,
        })?;
        if let Err(reason) = self.bind(spawned.child_mut()) {
            return Err(OwnedSpawnFailure {
                reason,
                outcome: SpawnFailure::Unbound {
                    spawned: Box::new(spawned),
                    error: io::Error::other("native constructor child bind refused"),
                },
            });
        }
        Ok(OwnedWorker {
            spawned: Some(spawned),
            _constructor: self,
        })
    }
}

fn before_birth(reason: Error, attempt: BirthAttempt) -> OwnedSpawnFailure {
    let outcome = match attempt.cancel() {
        Ok(proof) => SpawnFailure::NotSpawned(Box::new(proof)),
        Err(error) => SpawnFailure::Refused(error),
    };
    OwnedSpawnFailure { reason, outcome }
}

impl OwnedWorker {
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
        if aterm_uds::process::group_presence(group) != aterm_uds::process::ProcessPresence::Absent
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "native cohort kernel absence remains unresolved",
            ));
        }
        self.spawned.take();
        Ok(())
    }
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
