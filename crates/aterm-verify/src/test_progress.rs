// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Diagnostic boundaries inside a test check. These observations carry no verdict: leaving a
//! phase says only that its scope returned, advanced or unwound, including on an early refusal.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::checks::Check;
use crate::event_log::EventLog;

static RUNS: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ScopeExit {
    Advanced,
    Returned,
    Unwound,
}

/// A supported END record's own metadata. It never joins a START or proves execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct End {
    pub(crate) check: Check,
    pub(crate) run: u64,
    pub(crate) sequence: u32,
    pub(crate) phase: Phase,
    pub(crate) scope_exit: ScopeExit,
    seconds_bits: u64,
}

// Known-field duplicates must be rejected before a general JSON map erases them.
#[derive(serde::Deserialize)]
struct EndWire {
    schema: u64,
    event: crate::event_log::EventKind,
    provisional: bool,
    sample_complete: bool,
    check: String,
    test_run: u64,
    phase_sequence: u32,
    phase: Phase,
    scope_exit: ScopeExit,
    seconds: f64,
}

#[derive(serde::Deserialize)]
struct StartWire {
    schema: u64,
    event: crate::event_log::EventKind,
    provisional: bool,
    sample_complete: bool,
    check: String,
    test_run: u64,
    phase_sequence: u32,
    #[serde(rename = "phase")]
    _phase: Phase,
    scope_exit: aterm_json::Value,
    seconds: aterm_json::Value,
}

/// Validate a producer's START observation, without inferring activity or an END.
pub(crate) fn supported_start(json: &str) -> bool {
    let Ok(wire) = aterm_json::from_str::<StartWire>(json) else {
        return false;
    };
    wire.schema == 1
        && wire.event == crate::event_log::EventKind::TestPhaseStarted
        && wire.provisional
        && !wire.sample_complete
        && [Check::Test, Check::Checkers]
            .into_iter()
            .any(|check| check.name() == wire.check)
        && wire.test_run != 0
        && wire.phase_sequence != 0
        && wire.scope_exit.is_null()
        && wire.seconds.is_null()
}

impl End {
    pub(crate) fn read(json: &str) -> Option<Self> {
        let wire: EndWire = aterm_json::from_str(json).ok()?;
        if wire.schema != 1
            || wire.event != crate::event_log::EventKind::TestPhaseEnded
            || !wire.provisional
            || wire.sample_complete
            || wire.test_run == 0
            || wire.phase_sequence == 0
            || !wire.seconds.is_finite()
            || wire.seconds < 0.0
        {
            return None;
        }
        let check = [Check::Test, Check::Checkers]
            .into_iter()
            .find(|check| check.name() == wire.check)?;
        Some(Self {
            check,
            run: wire.test_run,
            sequence: wire.phase_sequence,
            phase: wire.phase,
            scope_exit: wire.scope_exit,
            seconds_bits: if wire.seconds == 0.0 {
                0.0f64.to_bits()
            } else {
                wire.seconds.to_bits()
            },
        })
    }

    pub(crate) fn seconds(&self) -> f64 {
        f64::from_bits(self.seconds_bits)
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
        self.leave(ScopeExit::Advanced);
        self.sequence += 1;
        self.current = Some((phase, Instant::now()));
        self.emit(
            phase,
            crate::event_log::EventKind::TestPhaseStarted,
            None,
            None,
        );
    }

    fn leave(&mut self, reason: ScopeExit) {
        if let Some((phase, began)) = self.current.take() {
            self.emit(
                phase,
                crate::event_log::EventKind::TestPhaseEnded,
                Some(reason),
                Some(began.elapsed().as_secs_f64()),
            );
        }
    }

    fn emit(
        &self,
        phase: Phase,
        event: crate::event_log::EventKind,
        reason: Option<ScopeExit>,
        seconds: Option<f64>,
    ) {
        // Fixed vocabulary only: no recorded argv, environment, paths or child output.
        let value = crate::event_log::event_value(
            event,
            aterm_json::json!({
                  "check": self.check.name(),
                "test_run": self.run, "phase_sequence": self.sequence, "phase": phase,
                "scope_exit": reason, "seconds": seconds,
                "provisional": true, "sample_complete": false,
            }),
        );
        let line = format!("verify-event: {value}");
        crate::event_log::emit_progress_batch(self.events, std::slice::from_ref(&line));
    }
}

impl Drop for Progress<'_> {
    fn drop(&mut self) {
        self.leave(if std::thread::panicking() {
            ScopeExit::Unwound
        } else {
            ScopeExit::Returned
        });
    }
}

#[cfg(test)]
#[path = "test_progress_tests.rs"]
mod tests;
