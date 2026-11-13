// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One bounded launchd cleanup protocol for ordinary jobs and native qualification.

use std::process::Output;
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Command;

/// Why cleanup did not establish that the original registration is absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupFailure {
    /// The native command could not be started or collected.
    Command,
    /// A native result did not establish the registration's state.
    Unknown,
    /// The common operation deadline expired.
    Deadline,
    /// The lab's stop interrupted observation; in a cleanup under way ([`crate::stop::Grace`]),
    /// a second signal cut it short.
    Interrupted,
}

/// A bounded cleanup failure; it never means the job was successfully removed.
#[derive(Debug)]
pub struct CleanupError {
    /// The failure category; no category establishes absence.
    pub kind: CleanupFailure,
    phase: &'static str,
    detail: String,
}

impl CleanupError {
    fn new(kind: CleanupFailure, phase: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            phase,
            detail: detail.into(),
        }
    }
    fn expired(phase: &'static str) -> Self {
        Self::new(
            CleanupFailure::Deadline,
            phase,
            "the common operation deadline expired",
        )
    }
}

impl std::fmt::Display for CleanupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "app-band {} {:?}: {}",
            self.phase, self.kind, self.detail
        )
    }
}
impl std::error::Error for CleanupError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Registration {
    Present,
    Absent,
}

/// Collect one command under the operation's absolute deadline, or until a second signal cuts
/// this thread's cleanup short ([`crate::stop::cut`]); the stop that asked for the cleanup does
/// not end it. The shared Unix runner bounds kill-group cleanup and reap instead of calling an
/// unbounded Child::wait. A timeout or a cut can include its finite native kill grace and
/// one-second reap grace.
pub(super) fn output(
    phase: &'static str,
    command: &mut Command,
    deadline: Instant,
) -> Result<Output, CleanupError> {
    if Instant::now() >= deadline {
        return Err(CleanupError::expired(phase));
    }
    aterm_uds::deadline::outputs_until_unless(
        &mut [command],
        deadline,
        Duration::from_secs(1),
        &mut crate::stop::cut,
    )
    .pop()
    .expect("one command has one result")
    .map_err(|why| {
        let kind = match why {
            aterm_uds::deadline::RunError::TimedOut if crate::stop::cut() => {
                CleanupFailure::Interrupted
            }
            aterm_uds::deadline::RunError::TimedOut => CleanupFailure::Deadline,
            _ => CleanupFailure::Command,
        };
        CleanupError::new(kind, phase, why.to_string())
    })
}

fn unknown(phase: &'static str, output: &Output) -> CleanupError {
    let bounded = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .chars()
            .take(2048)
            .collect::<String>()
    };
    CleanupError::new(
        CleanupFailure::Unknown,
        phase,
        format!(
            "registration remains unknown ({}): stdout={:?}; stderr={:?}",
            output.status,
            bounded(&output.stdout),
            bounded(&output.stderr)
        ),
    )
}

fn after_bootout(mut error: CleanupError, removed: &Output) -> CleanupError {
    error.detail = format!(
        "{}; prior bootout result: {}",
        error.detail,
        unknown("bootout", removed).detail
    );
    error
}

pub(super) fn decode_registration(
    output: &Output,
    label: &str,
    uid: u32,
) -> Result<Registration, CleanupError> {
    if output.status.success() {
        return Ok(Registration::Present);
    }
    // Native absence is exact to the owned service/domain and diagnostic. Permission,
    // transport and malformed-command failures supply no absence evidence.
    let missing =
        format!("Bad request.\nCould not find service \"{label}\" in domain for user gui: {uid}\n");
    if output.status.code() == Some(113)
        && output.stdout.is_empty()
        && output.stderr == missing.as_bytes()
    {
        Ok(Registration::Absent)
    } else {
        Err(unknown("print", output))
    }
}

trait Control {
    fn now(&mut self) -> Instant;
    fn registration(
        &mut self,
        label: &str,
        deadline: Instant,
    ) -> Result<Registration, CleanupError>;
    fn bootout(&mut self, label: &str, deadline: Instant) -> Result<Output, CleanupError>;
    fn pause(&mut self) -> Result<(), CleanupError>;
}

struct Live;
impl Control for Live {
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    fn registration(
        &mut self,
        label: &str,
        deadline: Instant,
    ) -> Result<Registration, CleanupError> {
        let mut command = aterm_uds::spawnfd::command(super::LAUNCHCTL);
        command.arg("print").arg(super::target(label));
        let observed = output("print", &mut command, deadline)?;
        decode_registration(&observed, label, crate::sys::getuid())
    }
    fn bootout(&mut self, label: &str, deadline: Instant) -> Result<Output, CleanupError> {
        let mut command = aterm_uds::spawnfd::command(super::LAUNCHCTL);
        command.arg("bootout").arg(super::target(label));
        output("bootout", &mut command, deadline)
    }
    fn pause(&mut self) -> Result<(), CleanupError> {
        crate::stop::sleep(Duration::from_millis(20)).map_err(|why| {
            CleanupError::new(CleanupFailure::Interrupted, "observe", format!("{why:?}"))
        })
    }
}

fn observe(
    control: &mut impl Control,
    label: &str,
    deadline: Instant,
) -> Result<Registration, CleanupError> {
    if control.now() >= deadline {
        return Err(CleanupError::expired("observe"));
    }
    control.registration(label, deadline)
}

fn remove_with(
    control: &mut impl Control,
    label: &str,
    deadline: Instant,
) -> Result<(), CleanupError> {
    if observe(control, label, deadline)? == Registration::Absent {
        return Ok(());
    }
    if control.now() >= deadline {
        return Err(CleanupError::expired("bootout"));
    }
    // A bootout refusal can race automatic retirement. One subsequent positive absence
    // observation settles it; never retry the mutation or turn arbitrary failure green.
    let removed = control.bootout(label, deadline)?;
    loop {
        if observe(control, label, deadline).map_err(|why| after_bootout(why, &removed))?
            == Registration::Absent
        {
            return Ok(());
        }
        if !removed.status.success() {
            return Err(unknown("bootout", &removed));
        }
        control.pause()?;
    }
}

pub(super) fn remove(label: &str, deadline: Instant) -> Result<(), CleanupError> {
    remove_with(&mut Live, label, deadline)
}

pub(super) fn await_absent(label: &str, deadline: Instant) -> Result<(), CleanupError> {
    let mut control = Live;
    loop {
        if observe(&mut control, label, deadline)? == Registration::Absent {
            return Ok(());
        }
        control.pause()?;
    }
}

#[cfg(test)]
#[path = "band_cleanup_tests.rs"]
mod tests;
