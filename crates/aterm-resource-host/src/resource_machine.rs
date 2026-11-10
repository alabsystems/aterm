// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One native machine observer, independent of any GUI or external command.
//! Observations confer no allocation, retained claim or scheduling credit.
use aterm_sysprobe::{CoreTicks, MemoryLevel, Probe, Throttle, ThrottleLevel};
use std::{
    io,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const CADENCE: Duration = Duration::from_secs(1);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pressure {
    Unknown,
    Normal,
    Warn,
    Critical,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Thermal {
    Unknown,
    Nominal,
    Fair,
    Serious,
    Critical,
}
#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    /// Zero only when the existing native CPU-count sources are unavailable.
    pub cpus: u32,
    pub busy_millicores: Option<u64>,
    /// Existing sysprobe free/reclaimable basis, never ledger claimable memory.
    pub available_memory_bytes: Option<u64>,
    pub pressure: Pressure,
    pub thermal: Thermal,
    pub sample_age: Duration,
    pub window: Duration,
}
struct Record {
    at: Instant,
    snapshot: Snapshot,
}
impl Record {
    fn at(&self, now: Instant) -> Snapshot {
        let mut snapshot = self.snapshot.clone();
        snapshot.sample_age = now.saturating_duration_since(self.at);
        snapshot
    }
}
#[derive(Default)]
struct State {
    stop: bool,
    latest: Option<Record>,
}
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}
/// Owns exactly one sampler worker. Drop interrupts its cadence wait and joins
/// only that worker; native probe calls retain their existing platform behavior.
pub(crate) struct CachedSampler {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}
impl CachedSampler {
    pub(crate) fn new() -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let owned = shared.clone();
        let worker = thread::Builder::new()
            .name("resource-machine".into())
            .spawn(move || run(owned))?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
    pub(crate) fn latest(&self) -> Option<Snapshot> {
        self.shared
            .state
            .lock()
            .ok()?
            .latest
            .as_ref()
            .map(|record| record.at(Instant::now()))
    }
}
impl Drop for CachedSampler {
    fn drop(&mut self) {
        match self.shared.state.lock() {
            Ok(mut state) => state.stop = true,
            Err(poisoned) => poisoned.into_inner().stop = true,
        }
        self.shared.wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn run(shared: Arc<Shared>) {
    let mut probe = Probe::new();
    let mut previous = None;
    loop {
        if shared.state.lock().map_or(true, |state| state.stop) {
            return;
        }
        let started = Instant::now();
        let ticks = probe.core_ticks();
        let at = Instant::now();
        let (snapshot, next) = sample(&mut probe, previous, ticks, at);
        previous = next;
        let Ok(mut state) = shared.state.lock() else {
            return;
        };
        if state.stop {
            return;
        }
        state.latest = Some(Record {
            at: started,
            snapshot,
        });
        let remaining = CADENCE.saturating_sub(started.elapsed());
        let Ok((state, _)) = shared
            .wake
            .wait_timeout_while(state, remaining, |state| !state.stop)
        else {
            return;
        };
        if state.stop {
            return;
        }
    }
}
type Previous = Option<(Instant, Vec<CoreTicks>)>;
fn sample(
    probe: &mut Probe,
    previous: Previous,
    ticks: Option<Vec<CoreTicks>>,
    at: Instant,
) -> (Snapshot, Previous) {
    let cpus = ticks
        .as_ref()
        .map(Vec::len)
        .filter(|count| *count > 0)
        .unwrap_or_else(|| aterm_sysprobe::cpu_layout().logical_cores());
    let window = previous.as_ref().map_or(Duration::ZERO, |(before, _)| {
        at.saturating_duration_since(*before)
    });
    let busy_millicores = if window.is_zero() {
        None
    } else {
        previous
            .as_ref()
            .and_then(|(_, before)| ticks.as_ref().and_then(|after| busy(before, after)))
    };
    let available_memory_bytes = aterm_sysprobe::memory_detail().map(|memory| memory.available);
    let pressure = pressure(aterm_sysprobe::memory_pressure());
    let thermal = aterm_sysprobe::process_thermal()
        .map(|reading| match reading.state {
            aterm_sysprobe::ProcessThermalState::Nominal => Thermal::Nominal,
            aterm_sysprobe::ProcessThermalState::Fair => Thermal::Fair,
            aterm_sysprobe::ProcessThermalState::Serious => Thermal::Serious,
            aterm_sysprobe::ProcessThermalState::Critical => Thermal::Critical,
        })
        .unwrap_or_else(|| thermal(probe.throttle().ok()));
    let next = ticks
        .filter(|ticks| !ticks.is_empty())
        .map(|ticks| (at, ticks));
    (
        Snapshot {
            cpus: u32::try_from(cpus).unwrap_or(0),
            busy_millicores,
            available_memory_bytes,
            pressure,
            thermal,
            sample_age: Duration::ZERO,
            window,
        },
        next,
    )
}
fn busy(before: &[CoreTicks], after: &[CoreTicks]) -> Option<u64> {
    if before.is_empty() || before.len() != after.len() {
        return None;
    }
    before.iter().zip(after).try_fold(0u64, |sum, (old, new)| {
        let user = new.user.checked_sub(old.user)?;
        let system = new.system.checked_sub(old.system)?;
        let nice = new.nice.checked_sub(old.nice)?;
        let idle = new.idle.checked_sub(old.idle)?;
        let active = u128::from(user) + u128::from(system) + u128::from(nice);
        let total = active + u128::from(idle);
        if total == 0 {
            return None;
        }
        sum.checked_add(u64::try_from(active.checked_mul(1000)? / total).ok()?)
    })
}
fn pressure(level: Option<MemoryLevel>) -> Pressure {
    match level {
        None => Pressure::Unknown,
        Some(MemoryLevel::Normal) => Pressure::Normal,
        Some(MemoryLevel::Warn) => Pressure::Warn,
        Some(MemoryLevel::Critical) => Pressure::Critical,
    }
}
fn thermal(reading: Option<Throttle>) -> Thermal {
    match reading
        .filter(|reading| reading.exact)
        .map(|reading| reading.level)
    {
        None => Thermal::Unknown,
        Some(ThrottleLevel::Nominal) => Thermal::Nominal,
        Some(ThrottleLevel::Light) => Thermal::Fair,
        Some(ThrottleLevel::Heavy) => Thermal::Serious,
        Some(ThrottleLevel::Severe) => Thermal::Critical,
    }
}
#[cfg(test)]
#[path = "resource_machine/tests.rs"]
mod tests;
