// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Neural Engine's power state (`docs/DESIGN-stats-deck-resources-2026-11-04.md` §3.1):
//! whether macOS keeps the engine powered, and how many cores it has, from the registry entry
//! its driver publishes (class `H11ANEIn`). macOS reports no busy share and no per-core reading,
//! so a reading says only on or off, and the engine stays on for a while after its work ends
//! (about 10 s on every chip measured under macOS 27).
//!
//! A runtime self-check decides whether it reads at all, with no list of chips or models: the
//! class matches, `ANEDevicePropertyNumANECores` reads a positive integer, each reading's
//! `CurrentPowerState` reads an integer from 0 to the same dictionary's `MaxPowerState`, and the
//! reader's own p95 over its first [`COST_READINGS`] readings stays within [`COST_LINE`]. The
//! first check that fails retires the reader for the process ([`Check`]), and it reads the
//! registry no more.

use std::time::{Duration, Instant};

/// The most one reading may cost at p95 over the first [`COST_READINGS`]: A1's line for an
/// IORegistry read (`docs/DESIGN-stats-deck-units-2026-11-04.md` §10).
pub const COST_LINE: Duration = Duration::from_micros(200);

/// How many readings the cost check judges, from the first.
pub const COST_READINGS: usize = 60;

/// What the engine's entry gives once, as the reader starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    /// `DeviceProperties.ANEDevicePropertyNumANECores`, where it reads an integer.
    pub cores: Option<i64>,
}

/// The engine's `IOPowerManagement` dictionary as one reading read it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Power {
    /// `CurrentPowerState`, where it reads an integer.
    pub current: Option<i64>,
    /// `MaxPowerState`, where it reads an integer.
    pub max: Option<i64>,
}

/// One reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The engine's cores, read once (16 on every chip measured).
    pub cores: u16,
    /// Whether its power state is above 0. Powered is not busy: it stays on after its work.
    pub powered: bool,
}

/// The self-check that failed, which retires the reader for the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// No registry entry of the engine's class matched: an Intel Mac has none.
    Matched,
    /// `ANEDevicePropertyNumANECores` read no positive integer a `u16` holds.
    Cores,
    /// `CurrentPowerState` read no integer from 0 to `MaxPowerState`.
    PowerState,
    /// The reader's own p95 over its first [`COST_READINGS`] readings was over [`COST_LINE`].
    Cost,
}

/// The self-check over one engine's readings: the start's checks, each reading's, and the cost
/// of the first [`COST_READINGS`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelfCheck {
    cores: u16,
    /// The first readings' costs, until [`COST_READINGS`] are judged; empty after.
    costs: Vec<Duration>,
    /// Whether the cost check has passed.
    proven: bool,
    failed: Option<Check>,
}

impl SelfCheck {
    /// Starts on the entry the engine's class matched, read once: the check that fails where
    /// none matched or its core count is no positive integer.
    ///
    /// # Errors
    ///
    /// [`Check::Matched`] for no entry, [`Check::Cores`] for a core count that is absent, not
    /// positive, or over `u16::MAX`.
    pub fn start(entry: Option<Entry>) -> Result<Self, Check> {
        let entry = entry.ok_or(Check::Matched)?;
        let cores = entry
            .cores
            .and_then(|n| u16::try_from(n).ok())
            .filter(|&n| n > 0)
            .ok_or(Check::Cores)?;
        Ok(Self {
            cores,
            costs: Vec::with_capacity(COST_READINGS),
            proven: false,
            failed: None,
        })
    }

    /// The check that failed, once one has.
    #[must_use]
    pub const fn failed(&self) -> Option<Check> {
        self.failed
    }

    /// Judges one reading of `power` that cost `cost`: the reading, or the check that failed,
    /// at this reading or an earlier one. The cost check judges the first [`COST_READINGS`]
    /// readings together, at the last of them.
    ///
    /// # Errors
    ///
    /// The check that failed: [`Check::PowerState`] for a state that is absent or outside 0 to
    /// its maximum, [`Check::Cost`] for a p95 over [`COST_LINE`]. Every later reading answers
    /// the same check.
    pub fn step(&mut self, power: Power, cost: Duration) -> Result<Reading, Check> {
        if let Some(check) = self.failed {
            return Err(check);
        }
        let failed = |this: &mut Self, check: Check| {
            this.failed = Some(check);
            this.costs = Vec::new();
            Err(check)
        };
        let powered = match (power.current, power.max) {
            (Some(current), Some(max)) if (0..=max).contains(&current) => current > 0,
            _ => return failed(self, Check::PowerState),
        };
        if !self.proven {
            self.costs.push(cost);
            if self.costs.len() == COST_READINGS {
                if p95(&mut self.costs) > COST_LINE {
                    return failed(self, Check::Cost);
                }
                self.proven = true;
                self.costs = Vec::new();
            }
        }
        Ok(Reading {
            cores: self.cores,
            powered,
        })
    }
}

/// The 95th percentile of `costs` by nearest rank: the smallest cost at or above 95 % of them,
/// so 3 of 60 may exceed it. Sorts `costs`.
fn p95(costs: &mut [Duration]) -> Duration {
    costs.sort_unstable();
    let rank = (costs.len() * 95).div_ceil(100).max(1);
    costs.get(rank - 1).copied().unwrap_or_default()
}

/// A reader's platform calls: the live registry ([`Source::native`]), or a test's.
#[derive(Clone, Copy, Debug)]
pub struct Source {
    /// The engine's entry, matched and read once; `None` where no entry matches.
    pub entry: fn() -> Option<Entry>,
    /// One reading of its power state.
    pub power: fn() -> Power,
}

impl Source {
    /// This machine's registry, where the OS has a reader: macOS. `None` on every other OS,
    /// where no reader runs (an NPU there waits on its own reader, A2 §3.1).
    #[must_use]
    pub fn native() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            Some(Self {
                entry: crate::macos::ane_entry,
                power: crate::macos::ane_power,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }
}

/// The Neural Engine reader: it starts on its first reading, times each reading's registry read,
/// and holds its self-check.
#[derive(Clone, Debug)]
pub struct Reader {
    source: Source,
    /// `None` until the first reading starts it.
    check: Option<Result<SelfCheck, Check>>,
}

impl Reader {
    /// A reader over `source`, not started: it matches nothing until it first reads.
    #[must_use]
    pub const fn on(source: Source) -> Self {
        Self {
            source,
            check: None,
        }
    }

    /// This machine's reader, where the OS has one ([`Source::native`]).
    #[must_use]
    pub fn native() -> Option<Self> {
        Source::native().map(Self::on)
    }

    /// One reading: the engine's power state, under its self-check. The first reading matches
    /// the engine and reads its core count; a reading after a failed check reads nothing.
    ///
    /// # Errors
    ///
    /// The check that failed ([`Check`]), now or at an earlier reading.
    pub fn read(&mut self) -> Result<Reading, Check> {
        let entry = self.source.entry;
        let check = match self.check.get_or_insert_with(|| SelfCheck::start(entry())) {
            Ok(check) => check,
            Err(failed) => return Err(*failed),
        };
        if let Some(failed) = check.failed() {
            return Err(failed);
        }
        let at = Instant::now();
        let power = (self.source.power)();
        check.step(power, at.elapsed())
    }
}

#[cfg(test)]
#[path = "neural_engine_tests.rs"]
mod tests;
