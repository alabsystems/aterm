// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pre-cleanup observations for an owned exchange's timeout, without its command or contents.

use std::fmt;

/// Which existing bound ended an owned exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnedInputTimeoutStage {
    /// The work deadline elapsed before execution and both output drains completed.
    Exchange,
    /// Windows' existing retirement grace elapsed after the exchange completed.
    Retirement,
}

/// What this waiter observed of the owned input before it began cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnedInputState {
    /// The child was not spawned, so no input exchange started.
    NotStarted,
    /// This streaming call supplied no owned input to track.
    Unmanaged,
    /// Some owned input had not been written and its writer remained open.
    Pending,
    /// Every owned input byte was written; empty input is complete without a write.
    Complete,
    /// The pipe closed or the root exited before every owned byte was written.
    ClosedEarly,
}

/// The observations preceding an owned exchange's cleanup, carried inside a `TimedOut` I/O error.
/// No command, argument, input or output contents are retained. EOF and natural exit are actual
/// observations, never consequences inferred from closing readers, termination or the cleanup wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnedInputTimeout {
    /// Which existing deadline expired.
    pub stage: OwnedInputTimeoutStage,
    /// Whether process creation returned an owned child.
    pub spawned: bool,
    /// Whether this waiter observed the root's natural exit before cleanup.
    pub natural_exit_observed: bool,
    /// Completion of this call's owned input, independently of root exit and output EOF.
    pub stdin: OwnedInputState,
    /// Whether a stdout read observed EOF; false before spawn, not an inferred empty stream.
    pub stdout_eof: bool,
    /// Whether a stderr read observed EOF; false before spawn, not an inferred empty stream.
    pub stderr_eof: bool,
    /// Owned input bytes actually written, independent of input that cleanup later scrubbed.
    pub stdin_bytes_sent: usize,
    /// Stdout bytes actually read, saturating at `usize::MAX` for an unbounded streaming call.
    pub stdout_bytes_observed: usize,
    /// Stderr bytes actually read, saturating at `usize::MAX` for an unbounded streaming call.
    pub stderr_bytes_observed: usize,
}

#[cfg(any(unix, windows, test))]
impl OwnedInputTimeout {
    pub(super) const fn before_start() -> Self {
        Self {
            stage: OwnedInputTimeoutStage::Exchange,
            spawned: false,
            natural_exit_observed: false,
            stdin: OwnedInputState::NotStarted,
            stdout_eof: false,
            stderr_eof: false,
            stdin_bytes_sent: 0,
            stdout_bytes_observed: 0,
            stderr_bytes_observed: 0,
        }
    }

    pub(super) fn spawned(input_len: Option<usize>) -> Self {
        let mut observed = Self {
            spawned: true,
            ..Self::before_start()
        };
        observed.observe_stdin(input_len, 0, true);
        observed
    }

    pub(super) fn observe_stdin(&mut self, input_len: Option<usize>, sent: usize, open: bool) {
        self.stdin_bytes_sent = sent;
        self.stdin = match input_len {
            None => OwnedInputState::Unmanaged,
            Some(len) if sent == len => OwnedInputState::Complete,
            Some(_) if open => OwnedInputState::Pending,
            Some(_) => OwnedInputState::ClosedEarly,
        };
    }

    #[cfg(any(unix, windows))]
    pub(super) fn into_io_error(self) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::TimedOut, self)
    }
}

impl fmt::Display for OwnedInputTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self.stage {
            OwnedInputTimeoutStage::Exchange => "exchange",
            OwnedInputTimeoutStage::Retirement => "retirement",
        };
        let stdin = match self.stdin {
            OwnedInputState::NotStarted => "not-started",
            OwnedInputState::Unmanaged => "unmanaged",
            OwnedInputState::Pending => "pending",
            OwnedInputState::Complete => "complete",
            OwnedInputState::ClosedEarly => "closed-early",
        };
        let output = |eof| {
            if !self.spawned {
                "not-started"
            } else if eof {
                "eof"
            } else {
                "pending"
            }
        };
        write!(
            f,
            "owned child {stage} timed out: spawned={} natural_exit_observed={} stdin={stdin} stdout={} stderr={} stdin_bytes_sent={} stdout_bytes_observed={} stderr_bytes_observed={}",
            self.spawned,
            self.natural_exit_observed,
            output(self.stdout_eof),
            output(self.stderr_eof),
            self.stdin_bytes_sent,
            self.stdout_bytes_observed,
            self.stderr_bytes_observed,
        )
    }
}

impl std::error::Error for OwnedInputTimeout {}

#[cfg(test)]
#[path = "owned_input_timeout_tests.rs"]
mod tests;
