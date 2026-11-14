// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A cooperative controller's original deadline and actual owned-group retirement.
//! No admission, reservation, SDK role or success receipt is created here.

use std::{
    io,
    time::{Duration, Instant},
};

use aterm_capacity::cohort::{Cohort, Lifetime};
use aterm_uds::spawnfd::{BirthAttempt, Command, SpawnFailure, Spawned};

use super::super::front::Front;
use super::ControllerOutcome;

const CANCEL_GRACE: Duration = Duration::from_secs(2);

/// A deadline refusal after the actual root was reaped and its native group
/// retired. This is diagnostic DATA, never a completion or resource grant.
#[derive(Debug)]
pub struct ControllerDeadline {
    root: std::process::ExitStatus,
}
impl ControllerDeadline {
    /// Preserve the actual owning wait status, including a zero-exit leader
    /// whose descendant outlived the original budget.
    pub const fn root_status(&self) -> std::process::ExitStatus {
        self.root
    }
}
impl std::fmt::Display for ControllerDeadline {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            out,
            "controller original deadline expired; actual root status {}; native group retired",
            self.root
        )
    }
}
impl std::error::Error for ControllerDeadline {}

/// Run one headless controller through its original deadline and actual group
/// retirement. Cancellation or expiry sends TERM/CONT, then KILL after one
/// cleanup grace, while the same unreaped leader still pins its group number.
/// The caller retains its original resources throughout this synchronous call.
/// No fresh operation deadline is created during launch, retry or retirement.
///
/// This is a cooperative dedicated-group contract. Deliberate group escapes
/// require separate custody from the fixed producer; no process census grants
/// completion. Only the captured cohort's native kernel absence closes it.
/// # Errors
/// Expired original budget, launch/observation/control/wait failure, observed
/// cancellation or deadline expiry. Errors return only after actual retirement;
/// unavailable custody retains resources rather than manufacturing completion.
pub fn supervise_controller_until(
    command: &mut Command,
    deadline: Instant,
    say: &mut dyn FnMut(&str),
) -> io::Result<ControllerOutcome> {
    supervise_with(command, deadline, say, &Instant::now)
}

fn supervise_with(
    command: &mut Command,
    deadline: Instant,
    say: &mut dyn FnMut(&str),
    now: &dyn Fn() -> Instant,
) -> io::Result<ControllerOutcome> {
    supervise_with_capture(command, deadline, say, now, &mut |spawned, _| {
        Cohort::of_spawned(spawned)
    })
}

fn supervise_with_capture(
    command: &mut Command,
    deadline: Instant,
    say: &mut dyn FnMut(&str),
    now: &dyn Fn() -> Instant,
    capture: &mut dyn FnMut(&mut Spawned, &Front) -> io::Result<Cohort>,
) -> io::Result<ControllerOutcome> {
    if now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    let front = Front::arm_cohort().ok_or_else(|| {
        io::Error::other("native controller signal forwarding could not be armed")
    })?;
    // Capture the original live leader before any target code can run. Restore
    // the caller's command policy on every return and unwind.
    struct Restore<'a> {
        command: &'a mut Command,
        suspended: bool,
    }
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            self.command.start_suspended(self.suspended);
        }
    }
    let suspended = command.starts_suspended();
    command.start_suspended(true);
    let restore = Restore { command, suspended };
    let (spawned, mut failure) = match restore.command.spawn_cohort(BirthAttempt::new()?) {
        Ok(spawned) => (spawned, None),
        Err(SpawnFailure::Unbound { spawned, error }) => (*spawned, Some(error)),
        Err(SpawnFailure::NotSpawned(proof)) => {
            front.never();
            return Err(proof.into_error());
        }
        Err(SpawnFailure::Refused(error)) => {
            say(&format!(
                "controller: retaining resources without an authoritative no-child receipt ({error})"
            ));
            loop {
                super::pause();
            }
        }
    };
    let mut owner = Owner {
        spawned,
        cohort: None,
        front: Some(front),
        closed: false,
    };
    let mut reported = false;
    loop {
        match capture(&mut owner.spawned, owner.front.as_ref().unwrap()) {
            Ok(cohort) => {
                owner.cohort = Some(cohort);
                break;
            }
            Err(error) => {
                if !reported {
                    say(&format!(
                        "controller: retaining original birth while cohort capture is unavailable ({error})"
                    ));
                    reported = true;
                }
                let cancelled = owner.front.as_ref().unwrap().observed().is_some();
                if cancelled || now() >= deadline {
                    failure.get_or_insert_with(|| {
                        if cancelled {
                            io::ErrorKind::Interrupted.into()
                        } else {
                            io::ErrorKind::TimedOut.into()
                        }
                    });
                    // The front has no target yet. Its queued cancellation
                    // still retires this exact stopped birth without GO/CONT.
                    let _ = owner.spawned.kill_group();
                }
                super::pause();
            }
        }
    }
    let mut may_have_started = false;
    let mut expired = false;
    if !owner.spawned.child_mut().is_reaped() {
        let pid = owner.spawned.child_mut().id();
        owner.front.as_ref().unwrap().aim(pid, None);
        expired = now() >= deadline;
        if expired || owner.front.as_ref().unwrap().observed().is_some() || failure.is_some() {
            // Capture consumed this same budget, or the attempt was cancelled
            // before GO. Never resume it, including by a cleanup SIGCONT.
            if let Err(error) = owner.spawned.kill_group() {
                failure.get_or_insert(error);
            }
        } else {
            // A resume error is not evidence that target code could not start.
            may_have_started = true;
            if let Err(error) = owner.spawned.child_mut().resume() {
                failure.get_or_insert(error);
            }
        }
    }
    let mut cancelling = None;
    let status = loop {
        expired |= now() >= deadline;
        let signalled = owner.front.as_ref().unwrap().observed().is_some();
        if (expired || signalled || failure.is_some()) && cancelling.is_none() {
            cancelling = Some(Instant::now());
            let controls = if may_have_started {
                vec![
                    owner.spawned.terminate_group(),
                    owner.spawned.continue_group(),
                ]
            } else {
                vec![owner.spawned.kill_group()]
            };
            for control in controls {
                if let Err(error) = control {
                    failure.get_or_insert(error);
                }
            }
        }
        if cancelling.is_some_and(|started| started.elapsed() >= CANCEL_GRACE)
            && let Err(error) = owner.spawned.kill_group()
        {
            failure.get_or_insert(error);
        }
        let child = owner.spawned.child_mut();
        if child.is_reaped() {
            break child.wait();
        }
        match child.exited() {
            Ok(true) => match owner.spawned.group_members_ended() {
                Ok(true) => {
                    owner.front.as_ref().unwrap().reaped();
                    break super::super::reap_accounted(owner.spawned.child_mut());
                }
                Ok(false) => {}
                Err(error) => {
                    failure.get_or_insert(error);
                }
            },
            Ok(_) => {}
            Err(error) => {
                failure.get_or_insert(error);
                if !owner.spawned.child_mut().owns_unreaped() {
                    owner.front.as_ref().unwrap().reaped();
                    break super::super::reap_accounted(owner.spawned.child_mut());
                }
            }
        }
        super::pause();
    };
    owner.front.as_ref().unwrap().reaped();
    // Enumeration above only chose when to perform the owning reap. It is
    // never drain authority. Retain the original budget through kernel absence.
    while owner.cohort.as_ref().unwrap().lifetime() != Lifetime::Drained {
        expired |= now() >= deadline;
        super::pause();
    }
    expired |= now() >= deadline;
    let signal = owner.front.take().unwrap().disarm_observed();
    owner.closed = true;
    let root = status?;
    if expired {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            ControllerDeadline { root },
        ));
    }
    if let Some(signal) = signal {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            format!(
                "controller cancellation {signal}; actual root status {root}; native group retired"
            ),
        ));
    }
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(ControllerOutcome { root, signal: None })
}

struct Owner {
    spawned: Spawned,
    cohort: Option<Cohort>,
    front: Option<Front>,
    closed: bool,
}
impl Drop for Owner {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        // Unwinding cannot surrender the caller's borrowed budget with live
        // target code. No user callback is invoked from this retirement path.
        while self.cohort.is_none() {
            let _ = self.spawned.kill_group();
            self.cohort = Cohort::of_spawned(&mut self.spawned).ok();
            if self.cohort.is_none() {
                super::pause();
            }
        }
        while self.spawned.child_mut().owns_unreaped() {
            let _ = self.spawned.kill_group();
            let ended = self.spawned.child_mut().exited().unwrap_or(false);
            if ended && self.spawned.group_members_ended().unwrap_or(false) {
                break;
            }
            super::pause();
        }
        if let Some(front) = &self.front {
            front.reaped();
        }
        let _ = super::super::reap_accounted(self.spawned.child_mut());
        while self.cohort.as_ref().unwrap().lifetime() != Lifetime::Drained {
            super::pause();
        }
    }
}

#[cfg(test)]
#[path = "build_admit_controller_until_tests.rs"]
mod tests;
