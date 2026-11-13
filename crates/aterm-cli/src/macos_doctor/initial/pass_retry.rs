// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A setup worker's own durable pace between its passes: how many passes in a row of the run in
//! force left a step to try again by itself (`SetupNext::Retry`, or a pass that failed as a
//! whole), and when the last of them ran. The worker waits [`SCHEDULE`] after each before its
//! next pass, with no count that turns the wait into a hold, and reads this record when it
//! starts, so a relaunch, an update or a reboot goes on from the same count rather than starting
//! the pauses over. A pass that leaves nothing to retry, a person's Check again or a fact the
//! park watches starts the count afresh. The run has one worker at a time, a window's or the
//! login job's ([`PassWorker::Run`]), and each goes on from the pace the other kept.

use super::{PROFILE, Path, Value, durable_directory, json, write_json};
use aterm_types::retry_schedule::Backoff;
use std::time::Duration;

/// The worker's pause after each pass that left a step to retry: a minute after the first, as
/// what a pass retries (a service starting, the network) mostly changes within minutes, twice
/// as long after each later one, and never more than an hour, so a step that fails all day is
/// tried once an hour, quietly, and finishes once whatever it waits on comes back.
pub const SCHEDULE: Backoff = Backoff::new(Duration::from_secs(60), Duration::from_secs(60 * 60));

/// Which setup worker a pace belongs to, its record beside the run's progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassWorker {
    /// The run's own worker, a window's or the login job's, which runs every stage its process
    /// may.
    Run,
}

impl PassWorker {
    /// Every worker, each with its own record.
    pub(super) const ALL: [Self; 1] = [Self::Run];

    /// The worker's record, beside the run's progress.
    pub(super) const fn record(self) -> &'static str {
        match self {
            Self::Run => "setup-retry.json",
        }
    }
}

/// The passes in a row of one run that left a step to retry, and when the last ran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PassRetry {
    /// Passes in a row that left a step to retry; 0: the worker's next pass goes at once.
    pub tries: u32,
    /// The last of them, in seconds since the epoch.
    pub at: u64,
}

impl PassRetry {
    /// The record after one more pass, at `now`, that left a step to retry. A pass the pause
    /// let come (it ran out) makes the next pause longer; one a wake brought before then starts
    /// the same pause again from `now`, so a stream of early wakes never lengthens it faster
    /// than the schedule's own pauses would.
    #[must_use]
    pub fn after_another(self, now: u64) -> Self {
        let ran_out = self.tries == 0 || now >= self.due(now);
        Self {
            tries: if ran_out {
                self.tries.saturating_add(1)
            } else {
                self.tries
            },
            at: now,
        }
    }

    /// When the worker's next pass is due ([`SCHEDULE`]), in seconds since the epoch, read at
    /// `now`: at once for a last pass recorded after `now`, the clock set back since
    /// ([`Backoff::due_by`]).
    #[must_use]
    pub fn due(self, now: u64) -> u64 {
        SCHEDULE.due_by(self.at, self.tries, now)
    }

    /// [`Self::due`] while a pass in a row left a step to retry; `None` when none did, and
    /// nothing of this pace times the worker's next pass.
    #[must_use]
    pub fn pending(self, now: u64) -> Option<u64> {
        (self.tries > 0).then(|| self.due(now))
    }
}

/// `worker`'s record of `revision`: none, one of another run, or one that cannot be read reads
/// as no pass retried, so the worker's next pass goes at once.
pub(super) fn load(root: &Path, worker: PassWorker, revision: u64) -> PassRetry {
    let read = || -> Option<PassRetry> {
        let bytes = crate::macos_doctor::read_record_bytes(&root.join(worker.record())).ok()?;
        let record: Value = aterm_json::from_slice(&bytes).ok()?;
        if record["profile"].as_str() != Some(PROFILE)
            || record["revision"].as_u64() != Some(revision)
        {
            return None;
        }
        Some(PassRetry {
            tries: u32::try_from(record["tries"].as_u64()?).ok()?,
            at: record["at"].as_u64()?,
        })
    };
    read().unwrap_or_default()
}

/// The record of the keyboard observers' Caps Lock worker, which no build since the login job
/// runs: nothing reads it, and the run's worker removes it as it saves its own pace.
const RETIRED: &str = "setup-input-retry.json";

/// Save `retry` as `worker`'s record of `revision`; a record of no try is removed, and so is the
/// retired worker's ([`RETIRED`]).
///
/// # Errors
/// The record cannot be written or removed.
pub(super) fn save(
    root: &Path,
    worker: PassWorker,
    revision: u64,
    retry: PassRetry,
) -> Result<(), String> {
    match std::fs::remove_file(root.join(RETIRED)) {
        Ok(()) => durable_directory(root).map_err(|error| error.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cannot remove the retired setup retry pause: {error}"
            ));
        }
    }
    if retry.tries == 0 {
        return clear(root, worker);
    }
    write_json(
        &root.join(worker.record()),
        &json!({
            "profile": PROFILE, "revision": revision, "tries": retry.tries, "at": retry.at,
        }),
    )
}

/// Remove `worker`'s record, whatever run it was of; every other worker's stays.
///
/// # Errors
/// The record exists and cannot be removed.
pub(super) fn clear(root: &Path, worker: PassWorker) -> Result<(), String> {
    match std::fs::remove_file(root.join(worker.record())) {
        Ok(()) => durable_directory(root).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot reset the setup retry pause: {error}")),
    }
}

#[cfg(test)]
#[path = "pass_retry_tests.rs"]
mod tests;
