// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One host-owned member borrows the original job's resources. Its original
//! child and descriptor capabilities survive errors until durable scope retirement.
use super::job::Events;
use super::start::clocks;
use aterm_capacity::cohort::launch::{CohortChild, CohortSignal, LaunchError};
use aterm_capacity::retained_cohort::CancelError;
use aterm_ctl::resource_admission_transport::Peer;
use aterm_types::resource_admission::{Binding, CompletionCause, Event, Outcome};
use aterm_uds::{exitwatch::ExitMoment, process, spawnfd};
use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::time::{Duration, Instant};

/// Borrowed original claim/fence/root access, never a second memory claim.
pub(super) trait MemberCustody {
    fn launch(&mut self, command: &mut spawnfd::Command) -> Result<CohortChild, LaunchError>;
    fn bind(&mut self, child: spawnfd::Spawned) -> Result<CohortChild, LaunchError>;
    fn cancel_no_child(&mut self, proof: spawnfd::NotSpawned) -> Result<(), CancelError>;
    fn retire(&mut self, child: &CohortChild) -> io::Result<()>;
    fn validate(&mut self) -> io::Result<()>;
}
/// The actual prepared CPU/GNU descriptors and one-use handoff binding.
pub(super) trait MemberResources {
    fn configure(&mut self, command: &mut spawnfd::Command) -> io::Result<()>;
    fn bind_child(&mut self, pid: u32) -> io::Result<()>;
}

#[derive(Clone, Debug)]
pub(super) struct Started {
    pub pid: u32,
    pub birth: u64,
    pub monotonic_ns: u64,
    pub unix_ns: u64,
}
#[derive(Clone, Debug)]
pub(super) struct Completed {
    pub outcome: Outcome,
    pub cause: CompletionCause,
    pub runtime_ns: u64,
}
pub(super) struct Progress {
    pub go_attempted: bool,
    pub launched: bool,
    pub ready_to_start: bool,
    pub completed: Option<Completed>,
    pub retired: bool,
    pub uncertain: bool,
    pub drained: bool,
}
enum State {
    NotAttempted,
    Unknown,
    NoChild(Option<Box<spawnfd::NotSpawned>>),
    Unbound(Option<Box<spawnfd::Spawned>>),
    Bound(Box<Bound>),
}
struct Bound {
    child: CohortChild,
    pid: u32,
    birth: u64,
    exit: Option<ExitMoment<Instant>>,
    ended: Option<Instant>,
    started: Option<Instant>,
    go_attempted: bool,
    launched: bool,
    reaped: bool,
    cleanup_at: Option<Instant>,
    cause: CompletionCause,
    completed: Option<Completed>,
}
impl Bound {
    fn new(mut child: CohortChild, wake: fn()) -> Self {
        let pid = child.child_mut().id();
        let birth = i32::try_from(pid)
            .ok()
            .and_then(process::read_birth)
            .map_or(0, |v| v.0);
        Self {
            child,
            pid,
            birth,
            exit: ExitMoment::watch_then(pid, Instant::now, wake),
            ended: None,
            started: None,
            go_attempted: false,
            launched: false,
            reaped: false,
            cleanup_at: None,
            cause: CompletionCause::Exited,
            completed: None,
        }
    }
    fn stop(&mut self, cause: CompletionCause) {
        if self.cleanup_at.is_none() || cause_priority(cause) > cause_priority(self.cause) {
            self.cause = cause;
            self.cleanup_at.get_or_insert_with(Instant::now);
            // ResourceLimit and any future non-natural policy cause kill
            // immediately. Cancellation/natural exit allow a short cleanup grace.
            let signal = if !self.go_attempted {
                CohortSignal::Kill
            } else if matches!(cause, CompletionCause::Exited | CompletionCause::Cancelled) {
                CohortSignal::Terminate
            } else {
                CohortSignal::Kill
            };
            let _ = self.child.request_group(signal);
        }
    }
}

/// The actor must keep this value until retired and its events drained. No
/// decoded PID, child exit, failed launch, or dropped resource proves closure.
#[must_use]
pub(super) struct OwnedAttempt<D> {
    binding: Binding,
    state: State,
    resources: Option<D>,
    events: Events,
    timeout: Option<Duration>,
    wake: fn(),
    retired: bool,
    uncertain: bool,
}
impl<D: MemberResources> OwnedAttempt<D> {
    /// The owner has durably journaled this member and emitted Queued. ARM/BIND
    /// are supplied by its original custody adapter. No target can GO here.
    pub fn launch(
        custody: &mut impl MemberCustody,
        mut command: spawnfd::Command,
        mut resources: D,
        events: Events,
        binding: Binding,
        timeout: Option<Duration>,
        wake: fn(),
    ) -> Self {
        let prepared = resources.configure(&mut command);
        command.start_suspended(true);
        let mut this = Self {
            binding,
            state: State::Unknown,
            resources: Some(resources),
            events,
            timeout,
            wake,
            retired: false,
            uncertain: false,
        };
        if prepared.is_err() || timeout == Some(Duration::ZERO) || this.events.cancelled() {
            this.state = State::NotAttempted;
            this.retired = true; // No ARM or native launch was called.
            this.resources.take();
            let _ = this.events.emit(Event::Refused {
                reason: "member preparation refused before ARM".into(),
            });
            return this;
        }
        this.accept_launch(custody.launch(&mut command));
        this
    }
    fn accept_launch(&mut self, result: Result<CohortChild, LaunchError>) {
        self.uncertain = false;
        self.state = match result {
            Ok(child) => State::Bound(Box::new(Bound::new(child, self.wake))),
            Err(LaunchError::NotSpawned(proof)) => State::NoChild(Some(proof)),
            Err(LaunchError::Unbound { spawned, .. }) => {
                self.uncertain = true;
                State::Unbound(Some(spawned))
            }
            Err(LaunchError::Refused(_)) => {
                self.uncertain = true;
                self.events.abandon();
                State::Unknown
            }
        };
    }
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    /// Observation for private BOUND journaling, never an owned-child capability.
    pub fn identity(&self) -> Option<(u32, u64)> {
        match &self.state {
            State::Bound(bound) if bound.birth != 0 => Some((bound.pid, bound.birth)),
            _ => None,
        }
    }
    /// Retain the registered group in aggregate accounting through retirement,
    /// including after its leader has been reaped. This grants no signal control.
    pub fn group(&self) -> Option<i32> {
        match &self.state {
            State::Bound(bound) if !self.retired => Some(bound.child.cohort().group()),
            _ => None,
        }
    }
    /// The actor has durably written BOUND/RUNNING and installed its member map.
    /// Validate the original current claim again immediately before actual GO.
    pub fn start(&mut self, custody: &mut impl MemberCustody) -> io::Result<Started> {
        let State::Bound(bound) = &mut self.state else {
            return Err(refused("member is not durably bound"));
        };
        if bound.go_attempted || bound.cleanup_at.is_some() {
            return Err(refused("member cannot start or replay"));
        }
        if bound.birth == 0 || bound.exit.is_none() {
            self.events.abandon();
            bound.stop(CompletionCause::Cancelled);
            return Err(refused("member identity or exit witness unavailable"));
        }
        let prepare = (|| {
            self.resources
                .as_mut()
                .ok_or_else(|| refused("member descriptors missing"))?
                .bind_child(bound.pid)?;
            custody.validate()?;
            let (monotonic_ns, unix_ns) = clocks()?;
            if self.events.cancelled() || bound.child.child_mut().exited()? {
                return Err(refused("member cancelled or exited before GO"));
            }
            // Capture before resume: a very fast child can publish its exit
            // before resume returns. Queue/durable journal time is excluded.
            bound.started = Some(Instant::now());
            bound.go_attempted = true;
            bound.child.child_mut().resume()?;
            bound.launched = true;
            Ok(Started {
                pid: bound.pid,
                birth: bound.birth,
                monotonic_ns,
                unix_ns,
            })
        })();
        match prepare {
            Ok(started) => {
                if self
                    .events
                    .emit(Event::Started {
                        pid: started.pid,
                        birth: started.birth,
                        monotonic_ns: started.monotonic_ns,
                        unix_ns: started.unix_ns,
                    })
                    .is_err()
                {
                    bound.stop(CompletionCause::Cancelled);
                }
                Ok(started)
            }
            Err(error) => {
                // Resume failure is not affirmative no-child/no-execution proof.
                self.events.abandon();
                bound.stop(CompletionCause::Cancelled);
                Err(error)
            }
        }
    }
    pub fn covers(&mut self, peer: Peer) -> bool {
        let State::Bound(bound) = &mut self.state else {
            return false;
        };
        if !bound.launched
            || bound.cleanup_at.is_some()
            || bound.reaped
            || bound.child.child_mut().exited().unwrap_or(true)
        {
            return false;
        }
        let root = process::Birth(bound.birth);
        let native = i32::try_from(bound.pid).ok().and_then(process::read_birth);
        native == Some(root)
            && process::stable_descendant(
                peer.pid,
                process::Birth(peer.birth),
                bound.pid,
                root,
                Instant::now() + Duration::from_millis(100),
            )
            && i32::try_from(bound.pid).ok().and_then(process::read_birth) == Some(root)
    }
    pub fn poll(
        &mut self,
        custody: &mut impl MemberCustody,
        cause: Option<CompletionCause>,
    ) -> Progress {
        self.events.flush();
        if let State::Unbound(spawned) = &mut self.state {
            if cause.is_some() || self.events.cancelled() {
                // This actual helper is held before GO. No target has run;
                // owned-child kill is safe even before durable group BIND.
                if let Some(child) = spawned.as_mut() {
                    let _ = child.child_mut().kill();
                }
            }
            if let Some(spawned) = spawned.take() {
                self.accept_launch(custody.bind(*spawned));
            }
        }
        if let State::NoChild(proof) = &mut self.state
            && let Some(proof) = proof.take()
        {
            // On an I/O failure the backend may retain the exact receipt;
            // this caller cannot turn that uncertainty into free capacity.
            match custody.cancel_no_child(*proof) {
                Ok(()) => {
                    self.retired = true;
                    self.uncertain = false;
                    self.resources.take();
                    let _ = self.events.emit(Event::Refused {
                        reason: "member target did not start".into(),
                    });
                }
                Err(error) => {
                    let (_, proof) = error.into_parts();
                    self.state = match proof {
                        Some(proof) => State::NoChild(Some(Box::new(proof))),
                        None => State::Unknown,
                    };
                    self.uncertain = true;
                    self.events.abandon();
                }
            }
        }
        if let State::Bound(bound) = &mut self.state {
            self.uncertain = poll_bound(bound, &mut self.events, self.timeout, cause);
            if bound.reaped && !self.retired && custody.retire(&bound.child).is_ok() {
                self.retired = true;
                self.resources.take();
                // Returning Finished lets the native caller reuse its borrowed
                // unit for another member. Never publish that handoff before
                // this exact scope retired and its host descriptor loan ended.
                // Runtime still ends at primary exit, excluding cleanup time.
                if let Some(complete) = &bound.completed {
                    let _ = self.events.emit(Event::Finished {
                        outcome: complete.outcome.clone(),
                        cause: complete.cause,
                        runtime_ns: complete.runtime_ns,
                    });
                }
            }
        }
        let (ready, completed) = match &self.state {
            State::Bound(bound) => (
                !bound.go_attempted && bound.cleanup_at.is_none(),
                bound.completed.clone(),
            ),
            _ => (false, None),
        };
        let (go_attempted, launched) = match &self.state {
            State::Bound(bound) => (bound.go_attempted, bound.launched),
            _ => (false, false),
        };
        Progress {
            go_attempted,
            launched,
            ready_to_start: ready,
            completed,
            retired: self.retired,
            uncertain: self.uncertain,
            drained: self.retired && self.events.drained(),
        }
    }
}
impl<D> Drop for OwnedAttempt<D> {
    fn drop(&mut self) {
        if !self.retired {
            if let State::Bound(bound) = &mut self.state
                && !bound.reaped
            {
                let _ = bound.child.request_group(CohortSignal::Kill);
            }
            if let State::Unbound(Some(child)) = &mut self.state {
                // The actual helper is held before GO; no target has run.
                let _ = child.child_mut().kill();
            }
            // Misuse must not return a borrowed CPU credit while an unknown
            // actual member may still run. The actor normally retains this
            // value; fail-closed debt lasts through the custodian lifetime.
            if let Some(resources) = self.resources.take() {
                std::mem::forget(resources);
            }
        }
    }
}

fn poll_bound(
    bound: &mut Bound,
    events: &mut Events,
    timeout: Option<Duration>,
    cause: Option<CompletionCause>,
) -> bool {
    if bound.reaped {
        return false;
    }
    let exited = match bound.child.child_mut().exited() {
        Ok(exited) => exited,
        Err(_) => {
            bound.stop(cause.unwrap_or(CompletionCause::Cancelled));
            events.abandon();
            return true;
        }
    };
    if bound.ended.is_none() {
        bound.ended = bound.exit.as_ref().and_then(ExitMoment::get);
    }
    if let Some(cause) = cause {
        bound.stop(cause);
    }
    if bound.cleanup_at.is_none() {
        let reason = if let (Some(start), Some(end)) = (bound.started, bound.ended) {
            Some(
                if timeout.is_some_and(|limit| end.saturating_duration_since(start) >= limit) {
                    CompletionCause::Timeout
                } else {
                    CompletionCause::Exited
                },
            )
        } else if exited {
            None
        }
        // Wait for the registered kernel exit stamp.
        else if bound
            .started
            .is_some_and(|start| timeout.is_some_and(|limit| start.elapsed() >= limit))
        {
            Some(CompletionCause::Timeout)
        } else if events.cancelled() {
            Some(CompletionCause::Cancelled)
        } else {
            None
        };
        if let Some(cause) = reason {
            bound.stop(cause);
        }
    }
    let others_exited = exited && crate::resource_memory::other_members_exited(bound.pid);
    if bound
        .cleanup_at
        .is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
    {
        // Escalate even if a TERM-ignoring leader is still alive. A failed
        // group signal never justifies losing its original unreaped pin.
        if bound.child.request_group(CohortSignal::Kill).is_err() && !others_exited {
            return true;
        }
    }
    if bound.cleanup_at.is_none() || !others_exited || (bound.launched && bound.ended.is_none()) {
        return false;
    }
    let status = match bound.child.child_mut().try_wait() {
        Ok(Some(status)) => status,
        Ok(None) => return false,
        Err(_) => {
            events.abandon();
            return true;
        }
    };
    bound.reaped = true;
    let outcome = match (status.code(), status.signal()) {
        (Some(code), _) => Some(Outcome::Exit(code)),
        (_, Some(signal)) => Some(Outcome::Signal(signal)),
        _ => None,
    };
    if bound.launched {
        if let (Some(outcome), Some(start), Some(end)) = (outcome, bound.started, bound.ended) {
            let complete = Completed {
                outcome,
                cause: bound.cause,
                runtime_ns: u64::try_from(end.saturating_duration_since(start).as_nanos())
                    .unwrap_or(u64::MAX),
            };
            bound.completed = Some(complete);
        } else {
            events.abandon();
        }
    } else {
        events.abandon();
    }
    false
}
fn cause_priority(cause: CompletionCause) -> u8 {
    if cause == CompletionCause::Exited {
        0
    } else if cause == CompletionCause::Cancelled {
        1
    } else if cause == CompletionCause::Timeout {
        2
    } else {
        3
    } // ResourceLimit is stronger than a pending graceful cancellation.
}
fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why)
}
#[cfg(test)]
#[path = "resource_admission_member/tests.rs"]
mod tests;
