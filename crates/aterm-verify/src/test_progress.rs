// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Diagnostic boundaries inside a test check. These observations carry no verdict: leaving a
//! phase says only that its scope returned, advanced or unwound, including on an early refusal.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::checks::Check;
use crate::event_log::EventLog;

static RUNS: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Preparation,
    RecordingAdmission,
    RecordingSetup,
    Recording,
    Planning,
    ExecutionAdmission,
    Probing,
    Dispatch,
    Reporting,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Preparation => "preparation",
            Self::RecordingAdmission => "recording-admission",
            Self::RecordingSetup => "recording-setup",
            Self::Recording => "recording",
            Self::Planning => "planning",
            Self::ExecutionAdmission => "execution-admission",
            Self::Probing => "probing",
            Self::Dispatch => "dispatch",
            Self::Reporting => "reporting",
        }
    }
}

/// A local sequence distinguishes repeated and concurrent test/checker runs inside the same
/// invocation. The enclosing EventLog supplies invocation attribution; neither ID is authority.
pub(crate) struct Progress<'a> {
    events: Option<&'a EventLog>,
    check: Check,
    run: u64,
    sequence: u32,
    current: Option<(Phase, Instant)>,
}

impl<'a> Progress<'a> {
    pub(crate) fn new(events: Option<&'a EventLog>, check: Check) -> Self {
        Self {
            events,
            check,
            run: RUNS.fetch_add(1, Ordering::Relaxed),
            sequence: 0,
            current: None,
        }
    }

    pub(crate) fn enter(&mut self, phase: Phase) {
        if self.current.is_some_and(|(current, _)| current == phase) {
            return;
        }
        self.leave("advanced");
        self.sequence += 1;
        self.current = Some((phase, Instant::now()));
        self.emit(phase, "test-phase-started", None, None);
    }

    fn leave(&mut self, reason: &'static str) {
        if let Some((phase, began)) = self.current.take() {
            self.emit(
                phase,
                "test-phase-ended",
                Some(reason),
                Some(began.elapsed().as_secs_f64()),
            );
        }
    }

    fn emit(&self, phase: Phase, event: &str, reason: Option<&str>, seconds: Option<f64>) {
        // Fixed vocabulary only: no recorded argv, environment, paths or child output.
        let value = aterm_json::json!({
            "schema": 1, "event": event, "check": self.check.name(),
            "test_run": self.run, "phase_sequence": self.sequence, "phase": phase.name(),
            "scope_exit": reason, "seconds": seconds,
            "provisional": true, "sample_complete": false,
        });
        let line = format!("verify-event: {value}");
        crate::event_log::emit_progress_batch(self.events, std::slice::from_ref(&line));
    }
}

impl Drop for Progress<'_> {
    fn drop(&mut self) {
        self.leave(if std::thread::panicking() {
            "unwound"
        } else {
            "returned"
        });
    }
}

#[cfg(test)]
#[path = "test_progress_tests.rs"]
mod tests;
