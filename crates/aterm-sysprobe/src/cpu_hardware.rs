// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only CPU hardware evidence. Kernel CPU accounting and hardware residency
//! are independent observations; neither is substituted for the other.

use std::time::Duration;
#[cfg(any(target_os = "macos", test))]
use std::time::Instant;

/// A bounded native reading, including a reason when the source did not answer.
pub type Reading<T> = Result<T, String>;

/// Whether this platform has a CPU Stats source. Where it has none, [`Probe::sample`] reads
/// nothing and answers `unsupported` at once, so a sample costs nothing.
pub const SOURCE: bool = cfg!(target_os = "macos");

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
    /// Read one interval without sleeping, starting a child, or changing policy. `gap` is the
    /// asking demand's gap (A2 §6.4): a snapshot more than `gap` after the previous one reads
    /// `warming` and starts the next interval, so no reading averages over a gap.
    pub fn sample(&mut self, gap: Duration) -> Sample {
        #[cfg(target_os = "macos")]
        {
            let began = Instant::now();
            let controls = native::controls();
            let reader = self.native.get_or_insert_with(native::Reader::new);
            let (window_ns, observation_span_ns, channels) = match reader {
                Ok(reader) => reader.sample(gap),
                Err(why) => (0, 0, Err(why.clone())),
            };
            Sample {
                captured_unix_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .and_then(|d| i64::try_from(d.as_millis()).ok()),
                collection_span_ns: nanos(began.elapsed()),
                window_ns,
                observation_span_ns,
                channels,
                controls,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = gap;
            Sample::unavailable("unsupported")
        }
    }

    /// Whether a sample taken now would read `warming` under `gap`: the source answers, but
    /// holds no snapshot within `gap`. A source that never subscribed is cold; one whose
    /// subscription failed is not, since a second sample would read the same failure.
    #[must_use]
    pub fn cold(&self, gap: Duration) -> bool {
        #[cfg(target_os = "macos")]
        {
            match &self.native {
                None => true,
                Some(Ok(reader)) => !reader.warm(gap),
                Some(Err(_)) => false,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = gap;
            false
        }
    }
}

/// The chip's DVFS range, MHz: each core kind's lowest and highest power-manager state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DvfsRange {
    /// The Super (performance) cores' lowest and highest state.
    pub p_mhz: (f64, f64),
    /// The Efficiency cores' lowest and highest state.
    pub e_mhz: (f64, f64),
}

/// Reads the chip's DVFS range from the power manager's tables (`IODeviceTree:/arm-io/pmgr`'s
/// `voltage-states1-sram` and `voltage-states5-sram`): one registry read, which the heat chain
/// takes once. `None` on a system with no such table (every one but macOS).
///
/// `Some(Err)` names the table that did not read (an Intel Mac has neither).
#[must_use]
pub fn dvfs_range() -> Option<Result<DvfsRange, &'static str>> {
    #[cfg(target_os = "macos")]
    {
        Some(native::dvfs_range())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The previous native snapshot of a cumulative-counter source, which the next one deltas
/// against, under the gap rule (A2 §6.4): a snapshot pairs with the previous one only within
/// its demand's gap. Past the gap, or with none held, it is kept only as the next one's
/// previous and its reading says `warming`, never an average over the gap.
#[cfg(any(target_os = "macos", test))]
struct Chain<T> {
    previous: Option<Link<T>>,
}

/// One native snapshot: when its call returned and how long the call took.
#[cfg(any(target_os = "macos", test))]
struct Link<T> {
    snapshot: T,
    after: Instant,
    call_ns: u64,
}

/// A snapshot paired with the previous one within its gap: what one delta reads.
#[cfg(any(target_os = "macos", test))]
struct Pair<'a, T> {
    /// The previous snapshot, no longer kept.
    previous: T,
    /// The snapshot just kept, the next one's previous.
    current: &'a T,
    /// The window between the two calls' returns, ns.
    window_ns: u64,
    /// The two calls' summed span, ns, which bounds the observation's skew.
    span_ns: u64,
}

#[cfg(any(target_os = "macos", test))]
impl<T> Default for Chain<T> {
    fn default() -> Self {
        Self { previous: None }
    }
}

#[cfg(any(target_os = "macos", test))]
impl<T> Chain<T> {
    /// Keeps `current` as the next snapshot's previous, and returns it paired with the previous
    /// one when the window between their returns is within `gap`.
    fn push(&mut self, current: Link<T>, gap: Duration) -> Option<Pair<'_, T>> {
        let previous = self.previous.take();
        let current = &*self.previous.insert(current);
        let previous = previous?;
        let window = current.after.saturating_duration_since(previous.after);
        (window <= gap).then(|| Pair {
            span_ns: previous.call_ns.saturating_add(current.call_ns),
            previous: previous.snapshot,
            current: &current.snapshot,
            window_ns: nanos(window),
        })
    }

    /// Forgets the previous snapshot, as a refused sample does.
    fn clear(&mut self) {
        self.previous = None;
    }

    /// Whether a snapshot taken at `now` would pair within `gap`.
    fn warm(&self, now: Instant, gap: Duration) -> bool {
        self.previous
            .as_ref()
            .is_some_and(|p| now.saturating_duration_since(p.after) <= gap)
    }
}

#[cfg(any(target_os = "macos", test))]
fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
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
#[path = "cpu_hardware_tests.rs"]
mod tests;
