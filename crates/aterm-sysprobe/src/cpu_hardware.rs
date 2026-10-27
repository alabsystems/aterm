// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only CPU hardware evidence. Kernel CPU accounting and hardware residency
//! are independent observations; neither is substituted for the other.

/// A bounded native reading, including a reason when the source did not answer.
pub type Reading<T> = Result<T, String>;

/// One IOReport state, with the source's cumulative-counter delta left intact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    /// Native state name.
    pub name: String,
    /// Residency delta in the channel's own units.
    pub ticks: i64,
}

/// One hardware channel. Names are not assumed to be kernel logical CPU IDs.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    /// Exact native RawElements bytes, retained for offline diagnosis.
    pub raw_elements: Vec<u8>,
    /// IOReport channel identity.
    pub name: String,
    /// IOReport subgroup, distinguishing cores from complexes.
    pub subgroup: String,
    /// Native residency unit label.
    pub unit: String,
    /// Raw states, retained even when interpretation is refused.
    pub states: Vec<State>,
    /// Fraction of the observed residency in an active state.
    pub active: Reading<f64>,
    /// MHz averaged over active residency, never over idle time.
    pub active_mhz: Reading<f64>,
    /// How frequency was interpreted, or why it was not.
    pub frequency_source: String,
}

/// Public power-manager limits. Missing fields stay missing, including on Macs
/// whose hardware frequency is restricted while thermal pressure is nominal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerLimits {
    /// Public speed limit percentage.
    pub speed_percent: Reading<u64>,
    /// Public scheduler-time limit percentage.
    pub scheduler_percent: Reading<u64>,
    /// Public available-processor count.
    pub available_cpus: Reading<u64>,
}

/// Point-in-time policy and topology observations alongside the hardware sample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Controls {
    /// OS product name and version, independently read for report identity.
    pub os_version: Reading<String>,
    /// Kernel release (Darwin version), independently read.
    pub kernel_release: Reading<String>,
    /// Full kernel build identity, independently read.
    pub kernel_version: Reading<String>,
    /// Online processor slots from the kernel's per-processor basic information.
    pub online: Reading<Vec<u32>>,
    /// Number of active logical processors from `hw.activecpu`.
    pub active_cpus: Reading<u64>,
    /// CLPC's requested recommended-core mask; not the final effective mask.
    pub requested_recommended_mask: Reading<u64>,
    /// Raw CLPC cluster masks; no assumption that binned bits are online CPUs.
    pub cluster_masks: Reading<Vec<(u32, u64)>>,
    /// The public power-manager limits or its exact failure code.
    pub power_limits: Reading<PowerLimits>,
    /// Last sleep time, unix milliseconds, when provided by the kernel.
    pub last_sleep_ms: Reading<i64>,
    /// Last wake time, unix milliseconds, when provided by the kernel.
    pub last_wake_ms: Reading<i64>,
    /// Historical root-domain sleep reason; not a current thermal verdict.
    pub last_sleep_reason: Reading<String>,
}

/// One IOReport interval and the policy readings made with it.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    /// Wall time when this collection completed, Unix milliseconds.
    pub captured_unix_ms: Option<i64>,
    /// Duration of this whole collection, including first subscription discovery.
    pub collection_span_ns: u64,
    /// Monotonic interval between completed native snapshots, ns.
    pub window_ns: u64,
    /// Sum of the two snapshot call durations; bounds their observation skew.
    pub observation_span_ns: u64,
    /// Hardware channels, or an explicit unavailable/refused/invalid reason.
    pub channels: Reading<Vec<Channel>>,
    /// Independent topology and power-manager observations.
    pub controls: Controls,
}

/// A CPU hardware sampler; initialization is lazy and a failed subscription is
/// retained rather than repeatedly probing an unsupported interface.
#[derive(Default)]
pub struct Probe {
    #[cfg(target_os = "macos")]
    native: Option<Reading<native::Reader>>,
}

impl Probe {
    /// Read one interval without sleeping, starting a child, or changing policy.
    pub fn sample(&mut self) -> Sample {
        #[cfg(target_os = "macos")]
        {
            let began = std::time::Instant::now();
            let controls = native::controls();
            let reader = self.native.get_or_insert_with(native::Reader::new);
            let (window_ns, observation_span_ns, channels) = match reader {
                Ok(reader) => reader.sample(),
                Err(why) => (0, 0, Err(why.clone())),
            };
            Sample {
                captured_unix_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .and_then(|d| i64::try_from(d.as_millis()).ok()),
                collection_span_ns: u64::try_from(began.elapsed().as_nanos()).unwrap_or(u64::MAX),
                window_ns,
                observation_span_ns,
                channels,
                controls,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Sample::unavailable("unsupported")
        }
    }
}

impl Sample {
    /// An explicitly unavailable sample, also used by an inert injected host.
    #[must_use]
    pub fn unavailable(reason: &str) -> Self {
        Self {
            captured_unix_ms: None,
            collection_span_ns: 0,
            window_ns: 0,
            observation_span_ns: 0,
            channels: Err(reason.to_owned()),
            controls: Controls {
                os_version: Err(reason.to_owned()),
                kernel_release: Err(reason.to_owned()),
                kernel_version: Err(reason.to_owned()),
                online: Err(reason.to_owned()),
                active_cpus: Err(reason.to_owned()),
                requested_recommended_mask: Err(reason.to_owned()),
                cluster_masks: Err(reason.to_owned()),
                power_limits: Err(reason.to_owned()),
                last_sleep_ms: Err(reason.to_owned()),
                last_wake_ms: Err(reason.to_owned()),
                last_sleep_reason: Err(reason.to_owned()),
            },
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn idle(name: &str) -> bool {
    matches!(name, "IDLE" | "DOWN" | "OFF")
}

#[cfg(any(target_os = "macos", test))]
fn active_state(name: &str) -> bool {
    let digits = |n: &str| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit());
    name.strip_prefix('P').is_some_and(digits)
        || name
            .strip_prefix('V')
            .and_then(|s| s.split_once('P'))
            .is_some_and(|(v, p)| digits(v) && digits(p))
}

/// Interpret all states or refuse. A zero total is unavailable; a valid all-idle
/// interval has zero activity but no active-frequency observation.
#[cfg(any(target_os = "macos", test))]
fn interpret(states: &[State], frequencies: Option<&[f64]>) -> (Reading<f64>, Reading<f64>) {
    let fail = |why: &str| (Err(why.to_owned()), Err(why.to_owned()));
    if states.is_empty() || states.iter().any(|s| s.ticks < 0) {
        return fail("unread_format:residency");
    }
    let Some(offset) = states.iter().position(|s| !idle(&s.name)) else {
        return fail("unread_format:active_states");
    };
    if states[offset..].iter().any(|s| !active_state(&s.name)) {
        return fail("unread_format:state_order");
    }
    let total: u128 = states.iter().map(|s| s.ticks as u128).sum();
    if total == 0 {
        return fail("no_counter_delta");
    }
    let active: u128 = states[offset..].iter().map(|s| s.ticks as u128).sum();
    let ratio = Ok(active as f64 / total as f64);
    if active == 0 {
        return (ratio, Err("not_sampled:idle".into()));
    }
    let Some(frequencies) = frequencies.filter(|fs| {
        fs.len() == states.len() - offset
            && fs.iter().all(|f| f.is_finite() && *f > 0.0)
            && states[offset..].iter().enumerate().all(|(i, s)| {
                s.name == format!("P{i}")
                    || s.name == format!("V{i}P{}", states.len() - offset - 1 - i)
            })
    }) else {
        return (ratio, Err("unread_format:frequency_table".into()));
    };
    let weighted = states[offset..]
        .iter()
        .zip(frequencies)
        .map(|(s, f)| s.ticks as f64 / active as f64 * f)
        .sum();
    (ratio, Ok(weighted))
}

#[cfg(target_os = "macos")]
#[path = "cpu_hardware_macos.rs"]
mod native;

#[cfg(test)]
mod tests {
    use super::*;
    fn states(values: &[(&str, i64)]) -> Vec<State> {
        values
            .iter()
            .map(|(name, ticks)| State {
                name: (*name).into(),
                ticks: *ticks,
            })
            .collect()
    }
    #[test]
    fn hardware_residency_keeps_idle_separate_from_active_frequency() {
        let s = states(&[("DOWN", 100), ("IDLE", 400), ("P0", 100), ("P1", 400)]);
        let (active, frequency) = interpret(&s, Some(&[1000.0, 2000.0]));
        assert_eq!(active, Ok(0.5));
        assert_eq!(frequency, Ok(1800.0));
        let native = states(&[("IDLE", 500), ("V0P1", 100), ("V1P0", 400)]);
        assert_eq!(
            interpret(&native, Some(&[1000.0, 2000.0])),
            (Ok(0.5), Ok(1800.0))
        );
        let (active, frequency) = interpret(&states(&[("IDLE", 100), ("P0", 0)]), Some(&[1000.0]));
        assert_eq!(active, Ok(0.0));
        assert!(frequency.is_err());
    }
    #[test]
    fn invalid_residency_and_incomplete_frequency_are_not_zero() {
        for s in [
            states(&[]),
            states(&[("IDLE", -1), ("P0", 2)]),
            states(&[("IDLE", 0), ("P0", 0)]),
            states(&[("P0", 5), ("IDLE", 3)]),
        ] {
            assert!(interpret(&s, Some(&[1000.0])).0.is_err(), "{s:?}");
        }
        let s = states(&[("IDLE", 2), ("P0", 3), ("P1", 5)]);
        assert!(interpret(&s, Some(&[1000.0])).1.is_err());
        assert!(interpret(&s, None).1.is_err());
        let out_of_order = states(&[("IDLE", 2), ("P1", 3), ("P0", 5)]);
        assert!(interpret(&out_of_order, Some(&[1000.0, 2000.0])).1.is_err());
    }
}
