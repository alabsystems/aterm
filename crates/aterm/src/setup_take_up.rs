// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An open window takes up the setup run another process admits or reopens once the window's own
//! setup worker returned (init robustness, the wave-2 review): a widened `aterm init` cut short, a
//! run limited to one group cut short, a completion whose reread no longer finds a vault or a
//! checkout ready. The login job runs every stage of such a run that needs nobody, but the person
//! stages (the Permissions stage's offers, the Messages and FaceTime sign-outs, the Mac Setup tab)
//! are a window's, and a window the person keeps open for days never launches again. So once the
//! launch's own startup is done, the window watches the setup records
//! (`aterm_cli::macos_doctor::SetupRecordsWatch`) and, at each change and on a timer, with no
//! setup of its own under way, reads whether an admitted run owes a pass; one that does is
//! resumed here as a launch resumes it, offering nothing again, at the screen, holding the window
//! lock so the job gives way at its next stage boundary. A take-up that ends with the same run
//! still owed waits the run's own pause (`aterm_cli::macos_doctor::PASS_RETRY_SCHEDULE`), longer
//! after each in a row, so nothing spins and nothing is given up. It raises nothing a launch would
//! not: no window, no offer, no prompt.

use std::time::{Duration, Instant};

/// What the window reads and does, which a test fakes.
pub(super) trait TakeUp {
    /// Arm or retain the records watch before the authoritative look. Returns whether changes
    /// can wake the wait.
    fn arm(&mut self) -> bool;
    /// Wait until the setup records may have moved, or `within` passed.
    fn wait(&mut self, within: Duration);
    /// Whether setup is under way in this process: a startup, or a run's worker, running or
    /// parked.
    fn setup_here(&mut self) -> bool;
    /// The admitted run that owes a pass (`aterm_cli::macos_doctor::initial_setup_resumable`).
    fn resumable(&mut self) -> Result<Option<u64>, String>;
    /// Resume the run as a launch does, at the screen, returning once that startup ended.
    fn resume(&mut self);
    /// The clock the pauses are kept on.
    fn now(&mut self) -> Instant;
    /// One aterm.log line, said once until another replaces it.
    fn say(&mut self, line: String);
}

/// The longest wait between two looks with no change in the records heard.
pub(super) const LOOK_AGAIN: Duration = Duration::from_secs(60 * 60);

/// What the take-ups left: the run the last one left still owed, how many in a row did so, and
/// when the next of that run may go.
#[derive(Debug, Default)]
pub(super) struct Taken {
    revision: Option<u64>,
    tries: u32,
    due: Option<Instant>,
}

/// One look: with no setup under way here and an admitted run owed, resume it, unless an earlier
/// take-up left that same run owed and its pause has not passed. Returns how long the next wait
/// may last.
pub(super) fn look(host: &mut impl TakeUp, taken: &mut Taken) -> Duration {
    if host.setup_here() {
        return LOOK_AGAIN;
    }
    let revision = match host.resumable() {
        Ok(Some(revision)) => revision,
        Ok(None) => {
            *taken = Taken::default();
            return LOOK_AGAIN;
        }
        Err(why) => {
            host.say(format!(
                "first-run setup: this window could not read whether a run is owed: {why}; it \
                 reads again at the records' next change"
            ));
            return LOOK_AGAIN;
        }
    };
    let now = host.now();
    if taken.revision == Some(revision)
        && let Some(due) = taken.due
        && now < due
    {
        return due - now;
    }
    host.say(format!(
        "first-run setup: run {revision} owes a pass and no setup runs in this window; it goes on \
         here"
    ));
    host.resume();
    match host.resumable() {
        Ok(Some(after)) if after == revision => {
            taken.tries = if taken.revision == Some(revision) {
                taken.tries.saturating_add(1)
            } else {
                1
            };
            taken.revision = Some(revision);
            let pause = aterm_cli::macos_doctor::PASS_RETRY_SCHEDULE.after(taken.tries);
            taken.due = Some(host.now() + pause);
            host.say(format!(
                "first-run setup: run {revision} still owes a pass after this window took it up; \
                 it goes on here again in {}s at the soonest",
                pause.as_secs()
            ));
            pause
        }
        _ => {
            *taken = Taken::default();
            LOOK_AGAIN
        }
    }
}

/// The window's take-up: a [`look`] at once, then after every change of the records heard and at
/// the latest when the last look said, for the window's life. Without a watch it reads again
/// within the first retry pause, so a missing records folder cannot hide a newly admitted run.
pub(super) fn take_up_with(host: &mut impl TakeUp) -> ! {
    let mut taken = Taken::default();
    loop {
        let watching = host.arm();
        let within = look(host, &mut taken);
        host.wait(if watching {
            within
        } else {
            within.min(aterm_cli::macos_doctor::PASS_RETRY_SCHEDULE.after(1))
        });
    }
}

#[cfg(test)]
#[path = "setup_take_up_tests.rs"]
mod tests;
