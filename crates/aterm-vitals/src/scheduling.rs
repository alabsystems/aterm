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
mod tests {
    use super::*;
    use crate::model::{ClusterView, CoreView, CpuView};

    fn waiting() -> Snapshot {
        Snapshot {
            procs_window_ns: Some(1_000_000_000),
            cores_window_ns: Some(1_000_000_000),
            global: crate::model::ScopeView {
                wait: Some(4.0),
                ..Default::default()
            },
            cpu: Some(CpuView {
                cores: vec![
                    CoreView {
                        measured: true,
                        ..CoreView::default()
                    };
                    4
                ],
                clusters: vec![ClusterView {
                    name: "Performance".into(),
                    short: "P".into(),
                    cores: vec![0, 1, 2, 3],
                }],
                ..CpuView::default()
            }),
            ..Snapshot::default()
        }
    }

    #[test]
    fn idle_performance_is_an_observation_even_without_backgrounding() {
        let mut s = waiting();
        assert_eq!(
            idle_performance(&s),
            Some(IdlePerformance {
                capacity: 4,
                busy: 0.0,
                wait: 4.0
            })
        );
        s.global.wait = Some(0.9);
        assert_eq!(idle_performance(&s), None);
        s.global.wait = Some(4.0);
        for c in &mut s.cpu.as_mut().unwrap().cores {
            c.load.raw = 0.3;
        }
        assert_eq!(idle_performance(&s), None);
    }

    #[test]
    fn partial_unknown_and_malformed_readings_do_not_invent_the_observation() {
        let s = waiting();
        let mut partial = s.clone();
        partial.machine_only = true;
        assert_eq!(idle_performance(&partial), None);
        let mut missing = s.clone();
        missing.procs_window_ns = None;
        assert_eq!(idle_performance(&missing), None);
        let mut no_p = s.clone();
        no_p.cpu.as_mut().unwrap().clusters[0].short = "E".into();
        assert_eq!(idle_performance(&no_p), None);
        let mut unknown = s.clone();
        unknown.global.wait = None;
        assert_eq!(idle_performance(&unknown), None);
        let mut nan = s.clone();
        nan.global.wait = Some(f32::NAN);
        assert_eq!(idle_performance(&nan), None);
        let mut bad = s;
        bad.cpu.as_mut().unwrap().clusters[0].cores.push(99);
        assert_eq!(idle_performance(&bad), None);
    }
}
