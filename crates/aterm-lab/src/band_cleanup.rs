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
    /// The lab's stop condition interrupted observation.
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

/// Collect one command under the operation's absolute deadline. The shared Unix runner
/// bounds kill-group cleanup and reap instead of calling an unbounded Child::wait.
/// A timeout can include its finite native kill grace and one-second reap grace.
pub(super) fn output(
    phase: &'static str,
    command: &mut Command,
    deadline: Instant,
) -> Result<Output, CleanupError> {
    if Instant::now() >= deadline {
        return Err(CleanupError::expired(phase));
    }
    aterm_uds::deadline::outputs_until(&mut [command], deadline, Duration::from_secs(1))
        .pop()
        .expect("one command has one result")
        .map_err(|why| {
            let kind = if matches!(why, aterm_uds::deadline::RunError::TimedOut) {
                CleanupFailure::Deadline
            } else {
                CleanupFailure::Command
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
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::os::unix::process::ExitStatusExt as _;

    fn result(code: i32) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: b"refused".to_vec(),
        }
    }
    struct Fake {
        now: Instant,
        end: Instant,
        observed: VecDeque<Result<Registration, CleanupError>>,
        removed: Option<Result<Output, CleanupError>>,
        commands: Vec<&'static str>,
        expire_on_pause: bool,
    }
    impl Fake {
        fn new(observed: Vec<Result<Registration, CleanupError>>, code: i32) -> Self {
            let now = Instant::now();
            Self {
                now,
                end: now + Duration::from_secs(60),
                observed: observed.into(),
                removed: Some(Ok(result(code))),
                commands: Vec::new(),
                expire_on_pause: false,
            }
        }
    }
    impl Control for Fake {
        fn now(&mut self) -> Instant {
            self.now
        }
        fn registration(
            &mut self,
            _: &str,
            deadline: Instant,
        ) -> Result<Registration, CleanupError> {
            assert_eq!(deadline, self.end);
            self.commands.push("print");
            self.observed.pop_front().expect("expected observation")
        }
        fn bootout(&mut self, _: &str, deadline: Instant) -> Result<Output, CleanupError> {
            assert_eq!(deadline, self.end);
            self.commands.push("bootout");
            self.removed.take().expect("only one mutation")
        }
        fn pause(&mut self) -> Result<(), CleanupError> {
            self.commands.push("pause");
            if self.expire_on_pause {
                self.now = self.end;
            }
            Ok(())
        }
    }

    #[test]
    fn absent_and_retirement_race_need_positive_evidence_without_repeating_mutation() {
        let mut absent = Fake::new(vec![Ok(Registration::Absent)], 1);
        let end = absent.end;
        remove_with(&mut absent, "owned", end).unwrap();
        assert_eq!(absent.commands, ["print"]);
        let mut race = Fake::new(vec![Ok(Registration::Present), Ok(Registration::Absent)], 1);
        let end = race.end;
        remove_with(&mut race, "owned", end).unwrap();
        assert_eq!(race.commands, ["print", "bootout", "print"]);
        let mut pending = Fake::new(
            vec![
                Ok(Registration::Present),
                Ok(Registration::Present),
                Ok(Registration::Absent),
            ],
            0,
        );
        let end = pending.end;
        remove_with(&mut pending, "owned", end).unwrap();
        assert_eq!(
            pending.commands,
            ["print", "bootout", "print", "pause", "print"]
        );
    }

    #[test]
    fn successful_bootout_is_not_absence_and_all_observations_share_one_deadline() {
        let mut pending = Fake::new(
            vec![Ok(Registration::Present), Ok(Registration::Present)],
            0,
        );
        pending.expire_on_pause = true;
        let end = pending.end;
        assert_eq!(
            remove_with(&mut pending, "owned", end).unwrap_err().kind,
            CleanupFailure::Deadline
        );
        assert_eq!(pending.commands, ["print", "bootout", "print", "pause"]);
    }

    #[test]
    fn unknown_probe_and_command_timeout_remain_failures() {
        let mut unknown = Fake::new(
            vec![Err(CleanupError::new(
                CleanupFailure::Unknown,
                "print",
                "permission refused",
            ))],
            0,
        );
        let end = unknown.end;
        assert_eq!(
            remove_with(&mut unknown, "owned", end).unwrap_err().kind,
            CleanupFailure::Unknown
        );
        assert_eq!(unknown.commands, ["print"]);
        let mut timeout = Fake::new(vec![Ok(Registration::Present)], 0);
        timeout.removed = Some(Err(CleanupError::expired("bootout")));
        let end = timeout.end;
        assert_eq!(
            remove_with(&mut timeout, "owned", end).unwrap_err().kind,
            CleanupFailure::Deadline
        );
        assert_eq!(timeout.commands, ["print", "bootout"]);
        let mut refused = Fake::new(
            vec![Ok(Registration::Present), Ok(Registration::Present)],
            1,
        );
        let end = refused.end;
        assert_eq!(
            remove_with(&mut refused, "owned", end).unwrap_err().kind,
            CleanupFailure::Unknown
        );
        assert_eq!(refused.commands, ["print", "bootout", "print"]);
    }
}
