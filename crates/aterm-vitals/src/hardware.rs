// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hardware observations and bounded anomaly evidence. Pure data supplied by the host.

use crate::model::{Snapshot, Thermal, Ticks};

/// Minimum busy fraction of every E core in a recorded underuse episode.
pub const EFFICIENCY_BUSY_MIN: f32 = 0.9;
/// Required continuous observation before an episode is retained.
pub const SUSTAIN_NS: u64 = 30_000_000_000;

/// A reading or the source's explicit unavailable/refused reason.
pub type Reading<T> = Result<T, String>;

/// One raw hardware residency state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    /// Native name.
    pub name: String,
    /// Counter delta in source units.
    pub ticks: i64,
}

/// An IOReport channel, kept separate from logical CPU IDs.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    /// Exact native RawElements bytes.
    pub raw_elements: Vec<u8>,
    /// Native channel name.
    pub name: String,
    /// Core or complex performance-state subgroup.
    pub subgroup: String,
    /// Native residency units.
    pub unit: String,
    /// Complete bounded raw state vector.
    pub states: Vec<State>,
    /// Active fraction of all residency.
    pub active: Reading<f64>,
    /// MHz averaged only over active residency.
    pub active_mhz: Reading<f64>,
    /// Frequency table association, or unavailable reason.
    pub frequency_source: String,
}

/// Public power-manager limits, not a statement that other limits do not exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerLimits {
    /// Reported speed limit percentage.
    pub speed_percent: Reading<u64>,
    /// Reported scheduler-time percentage.
    pub scheduler_percent: Reading<u64>,
    /// Reported available-CPU count.
    pub available_cpus: Reading<u64>,
}

/// Independent current settings and historical sleep information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Controls {
    /// Reported OS product/version.
    pub os_version: Reading<String>,
    /// Kernel release.
    pub kernel_release: Reading<String>,
    /// Full kernel build identity.
    pub kernel_version: Reading<String>,
    /// Online kernel processor slots, independent of a recommended-core mask.
    pub online: Reading<Vec<u32>>,
    /// Active logical processors.
    pub active_cpus: Reading<u64>,
    /// Requested CLPC mask; the effective post-powerdown mask is not exposed here.
    pub requested_recommended_mask: Reading<u64>,
    /// Raw CLPC cluster masks, including any binned capacity bits.
    pub cluster_masks: Reading<Vec<(u32, u64)>>,
    /// Public limits or the actual read failure.
    pub power_limits: Reading<PowerLimits>,
    /// Last sleep in Unix milliseconds.
    pub last_sleep_ms: Reading<i64>,
    /// Last wake in Unix milliseconds.
    pub last_wake_ms: Reading<i64>,
    /// Historical sleep reason, never a current throttle diagnosis.
    pub last_sleep_reason: Reading<String>,
}

/// A bounded native hardware interval.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    /// Wall time when collection completed, independently of the core snapshot.
    pub captured_unix_ms: Option<i64>,
    /// Duration of this collection, including optional subscription discovery.
    pub collection_span_ns: u64,
    /// Time between completed native snapshots.
    pub window_ns: u64,
    /// Combined duration of those calls: their observation skew bound.
    pub observation_span_ns: u64,
    /// IOReport channels or explicit source failure.
    pub channels: Reading<Vec<Channel>>,
    /// Power and topology observations.
    pub controls: Controls,
}

impl Sample {
    /// A fully explicit unavailable observation (including inert host probes).
    #[must_use]
    pub fn unavailable(reason: &str) -> Self {
        Self {
            captured_unix_ms: None,
            collection_span_ns: 0,
            window_ns: 0,
            observation_span_ns: 0,
            channels: Err(reason.into()),
            controls: Controls {
                os_version: Err(reason.into()),
                kernel_release: Err(reason.into()),
                kernel_version: Err(reason.into()),
                online: Err(reason.into()),
                active_cpus: Err(reason.into()),
                requested_recommended_mask: Err(reason.into()),
                cluster_masks: Err(reason.into()),
                power_limits: Err(reason.into()),
                last_sleep_ms: Err(reason.into()),
                last_wake_ms: Err(reason.into()),
                last_sleep_reason: Err(reason.into()),
            },
        }
    }
}

/// One worker's policy and CPU-accounting evidence, without arguments or environment.
#[derive(Clone, Debug, PartialEq)]
pub struct Worker {
    /// The process ID, scoped by its birth and accounting generation.
    pub pid: u32,
    /// Process birth time when read.
    pub birth_ms: Option<i64>,
    /// Policy and lifetime QoS counters.
    pub scheduling: Option<crate::scheduling::Scheduling>,
    /// Self and reaped-child counters from the same process reading.
    pub cpu_accounting: crate::model::CpuAccounting,
}

impl From<&crate::model::ProcView> for Worker {
    fn from(p: &crate::model::ProcView) -> Self {
        Self {
            pid: p.pid,
            birth_ms: p.birth_ms,
            scheduling: p.scheduling,
            cpu_accounting: p.cpu_accounting,
        }
    }
}

/// One preserved anomaly frame, deliberately excluding command lines and environment.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Logical CPU membership as observed in this frame.
    pub clusters: Vec<crate::model::ClusterView>,
    /// Current busy fraction per logical CPU; unknown counters remain unknown.
    pub busy: Vec<Option<f32>>,
    /// This frame's kernel-counter window.
    pub kernel_window_ns: Option<u64>,
    /// Monotonic timestamp of the observation.
    pub at_ns: u64,
    /// Wall time of the observation.
    pub at_unix_ms: i64,
    /// Kernel cumulative busy/total counters in logical CPU order.
    pub counters: Vec<Ticks>,
    /// Independent hardware measurement.
    pub hardware: Option<Sample>,
    /// Public thermal state and low-power setting; nominal is not a no-limit proof.
    pub thermal: Option<Thermal>,
    /// Current runnable wait in cores.
    pub wait: Option<f32>,
    /// Worker identity, policy and CPU counters read with this frame.
    pub workers: Vec<Worker>,
}

/// A sustained E-busy/P-underused observation, retained after recovery for export.
#[derive(Clone, Debug, PartialEq)]
pub struct Bundle {
    /// Schema version; raw fields retain their original units.
    pub version: u32,
    /// Monotonic first matching observation.
    pub since_ns: u64,
    /// Latest matching observation.
    pub last_ns: u64,
    /// Recovery observation, if one has since been measured.
    pub resolved_ns: Option<u64>,
    /// At most eight frames, first and most recent observations retained.
    pub frames: Vec<Frame>,
}

/// Bounded recorder; it performs no I/O or actions.
#[derive(Debug, Default)]
pub(crate) struct Recorder {
    pending: Option<Bundle>,
    last: Option<Bundle>,
}

fn all_e_busy_p_idle(s: &Snapshot) -> Option<bool> {
    if s.machine_only
        || s.procs_window_ns.is_none()
        || s.procs_window_ns != s.cores_window_ns
        || !s.global.wait.is_some_and(f32::is_finite)
    {
        return None;
    }
    let cpu = s.cpu.as_ref()?;
    if cpu.cores.iter().any(|c| c.current().is_none()) {
        return None;
    }
    let e: Vec<_> = cpu
        .clusters
        .iter()
        .filter(|c| c.short == "E")
        .flat_map(|c| &c.cores)
        .collect();
    if e.is_empty() || !cpu.clusters.iter().any(|c| c.short == "P") {
        return None;
    }
    Some(
        crate::scheduling::idle_performance(s).is_some()
            && e.into_iter().all(|i| {
                cpu.cores
                    .get(*i)
                    .and_then(|c| c.current())
                    .is_some_and(|v| v >= EFFICIENCY_BUSY_MIN)
            }),
    )
}

impl Recorder {
    /// Whether an E-busy, P-idle observation is pending: the host's demand for a CPU Stats sample
    /// with each reading while it holds (A2 §6.4), so the frames after its first carry hardware.
    pub(crate) const fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn observe(&mut self, s: &Snapshot, counters: Option<&[Ticks]>) -> Option<Bundle> {
        let Some(anomaly) = all_e_busy_p_idle(s) else {
            self.pending = None;
            return self.last.clone();
        };
        if !anomaly {
            self.pending = None;
            if let Some(last) = &mut self.last {
                last.resolved_ns.get_or_insert(s.at_ns);
            }
            return self.last.clone();
        }
        let frame = Frame {
            clusters: s
                .cpu
                .as_ref()
                .map(|c| c.clusters.clone())
                .unwrap_or_default(),
            busy: s
                .cpu
                .as_ref()
                .map(|c| c.cores.iter().map(|c| c.current()).collect())
                .unwrap_or_default(),
            kernel_window_ns: s.cores_window_ns,
            at_ns: s.at_ns,
            at_unix_ms: s.at_unix_ms,
            counters: counters.unwrap_or_default().to_vec(),
            hardware: s.hardware.clone(),
            thermal: s.thermal,
            wait: s.global.wait,
            workers: s.procs.iter().take(64).map(Worker::from).collect(),
        };
        let b = self.pending.get_or_insert_with(|| Bundle {
            version: 1,
            since_ns: s.at_ns,
            last_ns: s.at_ns,
            resolved_ns: None,
            frames: Vec::new(),
        });
        if s.at_ns.saturating_sub(b.last_ns) > 10_000_000_000 {
            b.since_ns = s.at_ns;
            b.frames.clear();
        }
        if b.frames
            .last()
            .is_none_or(|f| s.at_ns.saturating_sub(f.at_ns) >= 1_000_000_000)
        {
            if b.frames.len() == 8 {
                b.frames.remove(1);
            }
            b.frames.push(frame);
        }
        b.last_ns = s.at_ns;
        if s.at_ns.saturating_sub(b.since_ns) >= SUSTAIN_NS {
            self.last = Some(b.clone());
        }
        self.last.clone()
    }
}

#[cfg(test)]
#[path = "hardware_tests.rs"]
mod tests;
