// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Measured CPU readiness and its provenance, separate from a resource reservation.

use std::time::Instant;

use aterm_json::{Value, json};
use aterm_sysprobe::CoreTicks;

/// Why a measurement cannot be qualified; unavailable is never zero utilization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingIssue {
    /// Only the first counter point has been observed.
    Warming,
    /// The sampled busy fraction fails the existing quiet CPU criterion.
    Busy,
    /// Component counters reset, overflowed or changed topology.
    CounterChanged,
    /// The platform cannot provide a complete CPU counter read.
    Unavailable,
    /// The bounded reservation has ended or its clock rolled back.
    Expired,
    /// An explicit immediate override cannot qualify measurements.
    Now,
    /// Patience forced admission cannot qualify measurements.
    Bound,
    /// The platform did not provide a finite, nonnegative load.
    LoadUnknown,
    /// Actual local admission provenance is missing or weak.
    UnknownAdmission,
    /// Observation retention failed before a measurement could start.
    Persistence,
    /// An interrupt ended the entry wait before measurement.
    Interrupted,
}

impl TimingIssue {
    pub(crate) const fn retryable(self) -> bool {
        matches!(self, Self::Warming | Self::Busy | Self::CounterChanged)
    }
}

impl std::fmt::Display for TimingIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Warming => "CPU observation warming",
            Self::Busy => "actual CPU is above the existing quiet criterion",
            Self::CounterChanged => "CPU counters reset or topology changed",
            Self::Unavailable => "actual CPU counters unavailable",
            Self::Expired => "timing reservation expired",
            Self::Now => "--admit now is not measurement qualification",
            Self::Bound => "forced admission is not measurement qualification",
            Self::LoadUnknown => "load observation unavailable",
            Self::UnknownAdmission => "timing admission provenance unavailable",
            Self::Persistence => {
                "measurement-entry observation could not be retained under its claim"
            }
            Self::Interrupted => "measurement-entry wait interrupted",
        })
    }
}

impl std::error::Error for TimingIssue {}

/// The requested quiet policy; an unknown wire policy still excludes other quiet work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QuietPolicy {
    /// Explicit load-only admission, including old records without a policy field.
    #[default]
    Load,
    /// CPU readiness must be observed at the actual measurement entry.
    Timing,
    /// An unrecognized policy, retained for resource exclusion but never qualifying.
    Unknown,
}

impl QuietPolicy {
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::Timing => "timing",
            Self::Unknown => "unknown",
        }
    }
}

/// One platform counter read. Its instant is local and never reconstructed from wall time.
#[derive(Clone, Debug)]
pub struct CpuPoint {
    /// Actual monotonic read time.
    pub at: Instant,
    /// Epoch time for provenance, not for calculating CPU or the sample duration.
    pub unix_ms: u64,
    /// Logical-core ordered cumulative provider ticks.
    pub ticks: Vec<CoreTicks>,
}

/// A checked CPU counter interval; ticks are provider units, never CPU nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuWindow {
    /// The observed interval's monotonic wall duration.
    pub interval_ns: u64,
    /// Epoch time of its second sample.
    pub observed_ms: u64,
    /// Number of logical cores read at both ends.
    pub cores: u32,
    /// Actual busy provider ticks across the cores.
    pub busy_ticks: u64,
    /// Actual total provider ticks across the cores.
    pub total_ticks: u64,
}

impl CpuWindow {
    /// Compare two complete samples, rejecting reset/overflow/topology/empty intervals.
    #[must_use]
    pub fn between(from: &CpuPoint, to: &CpuPoint) -> Option<Self> {
        if from.ticks.is_empty() || from.ticks.len() != to.ticks.len() {
            return None;
        }
        let interval_ns = u64::try_from(to.at.checked_duration_since(from.at)?.as_nanos())
            .ok()
            .filter(|n| *n > 0)?;
        let mut busy = 0u64;
        let mut idle = 0u64;
        for (a, b) in from.ticks.iter().zip(&to.ticks) {
            // Check every component before summing: one reset must not be masked by another's growth.
            busy = busy
                .checked_add(b.user.checked_sub(a.user)?)?
                .checked_add(b.system.checked_sub(a.system)?)?
                .checked_add(b.nice.checked_sub(a.nice)?)?;
            idle = idle.checked_add(b.idle.checked_sub(a.idle)?)?;
        }
        let total_ticks = busy.checked_add(idle).filter(|n| *n > 0)?;
        Some(Self {
            interval_ns,
            observed_ms: to.unix_ms,
            cores: u32::try_from(to.ticks.len()).ok()?,
            busy_ticks: busy,
            total_ticks,
        })
    }

    /// Actual sampled machine busy, using the existing quiet preset's CPU criterion.
    #[must_use]
    pub fn ready(self) -> bool {
        self.valid()
            && aterm_vitals::sys::predicate::quiet_cpu_percent(
                self.busy_ticks as f64 * 100.0 / self.total_ticks as f64,
            )
    }

    fn valid(self) -> bool {
        self.interval_ns > 0
            && self.cores > 0
            && self.total_ticks > 0
            && self.busy_ticks <= self.total_ticks
    }

    pub(crate) fn value(self) -> Value {
        json!({"interval_ns":self.interval_ns,"observed_ms":self.observed_ms,"cores":self.cores,
            "busy_ticks":self.busy_ticks,"total_ticks":self.total_ticks,"provider":"sysprobe_core_ticks",
            "scope":"sampled_global_cpu_window;not_continuous_quiet"})
    }

    pub(crate) fn read(value: &Value) -> Option<Self> {
        if value.get("provider")?.as_str()? != "sysprobe_core_ticks" {
            return None;
        }
        let number = |key| value.get(key)?.as_u64();
        let window = Self {
            interval_ns: number("interval_ns")?,
            observed_ms: number("observed_ms")?,
            cores: u32::try_from(number("cores")?).ok()?,
            busy_ticks: number("busy_ticks")?,
            total_ticks: number("total_ticks")?,
        };
        window.valid().then_some(window)
    }
}

/// How resource admission occurred; none of these words is itself a CPU observation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimingCause {
    /// Legacy, malformed, adopted or otherwise missing admission provenance.
    #[default]
    Unknown,
    /// Ordinary wait admission, still requiring a fresh measurement-entry observation.
    Wait,
    /// Explicit `--admit now` is diagnostic-only.
    Now,
    /// Patience forced admission is diagnostic-only.
    Bound,
}

/// Compact evidence appended under the existing claim's guard; absence never proves readiness.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimingEvidence {
    /// Actual resource admission mode.
    pub cause: TimingCause,
    /// Actual admission time, only when explicitly recorded (not a legacy timestamp fallback).
    pub admitted_ms: Option<u64>,
    /// CPU observed for resource admission, never relabeled as a postcompile entry window.
    pub admission: Option<CpuWindow>,
    /// Fresh CPU observation at the measurement entry; no parent sample is relabeled.
    pub entry: Option<CpuWindow>,
    /// Actual load at resource admission, never relabeled as entry load.
    pub admission_load1: Option<f64>,
    /// Actual load read at measurement entry; missing is not zero.
    pub entry_load1: Option<f64>,
}

impl TimingEvidence {
    pub(crate) fn value(self) -> Value {
        let cause = match self.cause {
            TimingCause::Unknown => "unknown",
            TimingCause::Wait => "wait",
            TimingCause::Now => "now",
            TimingCause::Bound => "bound",
        };
        json!({"cause":cause,"admitted_ms":self.admitted_ms,"admission":self.admission.map(CpuWindow::value),"entry":self.entry.map(CpuWindow::value),"admission_load1":self.admission_load1,"entry_load1":self.entry_load1})
    }

    pub(crate) fn read(value: &Value) -> Self {
        let cause = match value.get("cause").and_then(Value::as_str) {
            Some("wait") => TimingCause::Wait,
            Some("now") => TimingCause::Now,
            Some("bound") => TimingCause::Bound,
            _ => return Self::default(),
        };
        let Some(admitted_ms) = value.get("admitted_ms").and_then(Value::as_u64) else {
            return Self::default();
        };
        let admission = match value.get("admission") {
            Some(Value::Null) => None,
            Some(v) => match CpuWindow::read(v) {
                Some(w) => Some(w),
                None => return Self::default(),
            },
            None => return Self::default(),
        };
        let entry = match value.get("entry") {
            Some(Value::Null) => None,
            Some(v) => match CpuWindow::read(v) {
                Some(w) => Some(w),
                None => return Self::default(),
            },
            None => return Self::default(),
        };
        let load = |key| match value.get(key) {
            Some(Value::Null) => Some(None),
            Some(v) => v.as_f64().filter(|n| n.is_finite() && *n >= 0.0).map(Some),
            None => None,
        };
        let (Some(admission_load1), Some(entry_load1)) =
            (load("admission_load1"), load("entry_load1"))
        else {
            return Self::default();
        };
        Self {
            cause,
            admitted_ms: Some(admitted_ms),
            admission,
            entry,
            admission_load1,
            entry_load1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn point(at: Instant, user: u64, idle: u64) -> CpuPoint {
        CpuPoint {
            at,
            unix_ms: 100,
            ticks: vec![CoreTicks {
                user,
                idle,
                ..CoreTicks::default()
            }],
        }
    }

    #[test]
    fn component_resets_cannot_be_hidden_by_growth_elsewhere() {
        let start = Instant::now();
        let mut before = point(start, 100, 100);
        before.ticks[0].system = 100;
        let mut after = point(start + Duration::from_secs(2), 99, 1000);
        after.ticks[0].system = 1000;
        assert!(CpuWindow::between(&before, &after).is_none());
        after.ticks[0].user = 100;
        assert!(CpuWindow::between(&before, &after).is_some());
        after.ticks.push(CoreTicks::default());
        assert!(CpuWindow::between(&before, &after).is_none());
    }

    #[test]
    fn zero_empty_and_overflow_intervals_are_unknown() {
        let now = Instant::now();
        let before = point(now, 0, 0);
        assert!(CpuWindow::between(&before, &before).is_none());
        assert!(CpuWindow::between(&before, &point(now + Duration::from_secs(1), 0, 0)).is_none());
        let mut after = point(now + Duration::from_secs(1), u64::MAX, 1);
        assert!(CpuWindow::between(&before, &after).is_none());
        after.ticks.clear();
        assert!(CpuWindow::between(&before, &after).is_none());
    }

    #[test]
    fn readiness_reuses_the_existing_strict_quiet_cpu_criterion() {
        let now = Instant::now();
        let before = point(now, 0, 0);
        for (busy, idle, ready) in [
            (24, 76, true),
            (25, 75, false),
            (0, 100, true),
            (100, 0, false),
        ] {
            let window =
                CpuWindow::between(&before, &point(now + Duration::from_secs(2), busy, idle))
                    .unwrap();
            assert_eq!(window.ready(), ready);
            assert_eq!(CpuWindow::read(&window.value()), Some(window));
        }
    }

    #[test]
    fn malformed_or_legacy_proof_never_retains_a_ready_observation() {
        let now = Instant::now();
        let window = CpuWindow::between(
            &point(now, 0, 0),
            &point(now + Duration::from_secs(2), 1, 99),
        )
        .unwrap();
        let evidence = TimingEvidence {
            cause: TimingCause::Wait,
            admitted_ms: Some(100),
            admission: Some(window),
            entry: Some(window),
            admission_load1: Some(1.0),
            entry_load1: Some(2.0),
        };
        assert_eq!(TimingEvidence::read(&evidence.value()), evidence);
        assert_eq!(TimingEvidence::read(&json!({})), TimingEvidence::default());
        let mut value = evidence.value();
        value.as_object_mut().unwrap().insert(
            "entry".into(),
            json!({"provider":"invented","total_ticks":100}),
        );
        assert_eq!(TimingEvidence::read(&value), TimingEvidence::default());
    }
}
