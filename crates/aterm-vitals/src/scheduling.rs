// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scheduling observations, kept separate from a claim about the scheduler's cause.

use crate::model::Snapshot;

/// Darwin process policy and lifetime CPU time, read together with the process counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scheduling {
    /// Unix nice value, or the read's errno.
    pub nice: Result<i32, i32>,
    /// Target application bit from BSD info.
    pub application: bool,
    /// Target internal requested-background bit, not effective QoS.
    pub background_internal: bool,
    /// Target external requested-background bit, not effective QoS.
    pub background_external: bool,
    /// Target effective suppression flag.
    pub suppressed: bool,
    /// Darwin role (zero is unspecified), or the read's errno.
    pub role: Result<i32, i32>,
    /// Nanoseconds at each class in [`QOS_CLASSES`], or the read's errno.
    /// `None` means the conversion clock was unavailable, not zero CPU time.
    pub qos_cpu_ns: Option<Result<[u64; 7], i32>>,
}

/// The kernel's lifetime CPU-time class order. These are not P/E residency counters.
pub const QOS_CLASSES: [&str; 7] = [
    "default",
    "maintenance",
    "background",
    "utility",
    "legacy",
    "user_initiated",
    "user_interactive",
];

/// Concurrent observations that merit looking at worker policy, without asserting a cause.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IdlePerformance {
    /// Performance core capacity.
    pub capacity: usize,
    /// Busy performance cores' worth.
    pub busy: f32,
    /// Lower bound on cores' worth of runnable wait.
    pub wait: f32,
}

/// Maximum mean P-core busy fraction for the underuse observation.
pub const PERFORMANCE_BUSY_MAX: f32 = 0.25;

/// Flag at least one core's runnable wait while named P cores are at most 25% busy.
/// Only a whole, current reading qualifies; nice/background values never suppress the flag.
#[must_use]
pub fn idle_performance(s: &Snapshot) -> Option<IdlePerformance> {
    let window = s.procs_window_ns?;
    if s.machine_only || window == 0 || s.cores_window_ns? != window {
        return None;
    }
    let cpu = s.cpu.as_ref()?;
    let mut capacity = 0;
    let mut busy = 0.0;
    for cluster in cpu.clusters.iter().filter(|c| c.short == "P") {
        for &index in &cluster.cores {
            let load = cpu.cores.get(index)?.current()?;
            if !load.is_finite() || !(0.0..=1.0).contains(&load) {
                return None;
            }
            capacity += 1;
            busy += load;
        }
    }
    let wait = s.global.wait?;
    (capacity > 0
        && wait.is_finite()
        && wait >= 1.0
        && busy <= capacity as f32 * PERFORMANCE_BUSY_MAX)
        .then_some(IdlePerformance {
            capacity,
            busy,
            wait,
        })
}

/// Guidance states an observation and a per-exec candidate, never a diagnosed cause or fix.
pub const GUIDANCE: &str = "runnable wait with idle performance cores; cause unproven. Inspect each worker's application bit, role, requested-background bits and lifetime QoS. nice=0/background=false do not rule out a QoS ceiling. On macOS, taskpolicy -a applies to the actual exec; an outer build wrapper's role does not survive fork, and -B only clears Darwin backgrounding. Preserve loader environment and job ownership; measure counter deltas before adopting a policy.";

/// Read failure word, preserving an OS error distinct from an unsupported measurement.
#[must_use]
pub fn error_word(errno: i32) -> String {
    match errno {
        1 | 13 => "refused".into(),
        3 => "gone".into(),
        45 => "unsupported".into(),
        n => format!("os_error:{n}"),
    }
}

#[cfg(test)]
#[path = "scheduling_tests.rs"]
mod tests;
