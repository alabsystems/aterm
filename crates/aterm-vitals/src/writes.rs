// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The last hour of bytes written per program and scope ([`WriteHour`]), with the time its
//! readings covered: what `sys disk` lists as `program` rows.
//!
//! A program is the processes of one name in one scope (aterm, a tab, the rest of the Mac), as
//! [`Writer`] groups them each sweep. The figure is a floor: the counter
//! (`ri_diskio_byteswritten`) is the I/O a process issued itself, so dirty pages the kernel
//! flushes later are not its, and what a process writes after its last sweep is lost unless a
//! sweep reads it as a zombie. No sweep runs while no deck is shown, so an hour figure is a rate
//! over the time the readings covered ([`ProgramHour::per_hour`]), never an hourly total.

use std::collections::{HashSet, VecDeque};

use crate::model::{ProcScope, TOP_WRITERS, Writer};
use crate::ring::Lands;

/// Milliseconds one slot covers: a minute.
pub const SLOT_MS: i64 = 60_000;
/// Slots the hour keeps.
pub const HOUR_SLOTS: i64 = 60;
/// Programs the hour keeps; past it, the smallest hour outside its scope's first [`TOP_WRITERS`]
/// makes room for a newcomer ([`WriteHour::note`]).
pub const WRITE_PROGRAMS: usize = 64;
/// Covered seconds under which [`ProgramHour::per_hour`] gives no rate: the deck draws an
/// absence there, never a zero.
pub const MIN_COVERED_S: f64 = 60.0;

/// One program's last hour ([`crate::model::RowFacts::hour_writers`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ProgramHour {
    /// The name its processes share (data, never an instruction).
    pub name: String,
    /// Whose they are.
    pub scope: ProcScope,
    /// Bytes they wrote in the hour's covered time.
    pub bytes: u64,
    /// The most of them that wrote in one sweep.
    pub n_max: u32,
}

impl ProgramHour {
    /// Bytes per hour over `covered_s` seconds of readings
    /// ([`crate::model::RowFacts::writes_covered_s`]); `None` under [`MIN_COVERED_S`].
    #[must_use]
    pub fn per_hour(&self, covered_s: f64) -> Option<f64> {
        (covered_s.is_finite() && covered_s >= MIN_COVERED_S)
            .then_some(self.bytes as f64 * 3600.0 / covered_s)
    }
}

/// One program's slots.
#[derive(Clone, Debug, PartialEq)]
struct Program {
    name: String,
    scope: ProcScope,
    /// `(slot key, bytes, the most processes in one sweep)`, oldest first, each key once.
    slots: Vec<(i64, u64, u32)>,
    /// The last [`WriteHour::note`] it wrote in.
    noted: u64,
}

impl Program {
    fn total(&self) -> u64 {
        self.slots
            .iter()
            .fold(0u64, |sum, s| sum.saturating_add(s.1))
    }

    fn add(&mut self, k: i64, bytes: u64, n: u32) {
        match self.slots.binary_search_by_key(&k, |s| s.0) {
            Ok(i) => {
                let s = &mut self.slots[i];
                s.1 = s.1.saturating_add(bytes);
                s.2 = s.2.max(n);
            }
            Err(i) => self.slots.insert(i, (k, bytes, n)),
        }
    }
}

/// The last hour of writes per program and scope, in one-minute slots keyed by the wall clock
/// by the rings' one back-step rule ([`crate::ring::lands`]): a reading up to [`crate::ring::CLAMP_BACK_S`]
/// behind the newest lands in the newest slot, and a longer step back moves the whole hour back
/// with it, a slot behind the one the reading opens.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WriteHour {
    /// The newest slot's key (unix minute); `None` before the first reading.
    open_k: Option<i64>,
    /// When the newest reading was taken, unix ms.
    last_ms: i64,
    /// `(slot key, ms)` the sweep windows that formed a rate covered, oldest first.
    covered: VecDeque<(i64, u64)>,
    /// At most [`WRITE_PROGRAMS`], each with bytes in the hour.
    programs: Vec<Program>,
    /// How many sweep windows [`Self::note`] has taken.
    notes: u64,
}

impl WriteHour {
    /// An hour with nothing read.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Move the hour to a reading taken at `at_unix_ms` and give the slot it lands in; what falls
    /// out of the hour goes, and a program with nothing left in it with it.
    pub fn at(&mut self, at_unix_ms: i64) -> i64 {
        let newest = self.open_k.map(|_| self.last_ms);
        let landing = crate::ring::lands(self.open_k, newest, at_unix_ms, SLOT_MS);
        let clamped_back = landing == Lands::Newest && at_unix_ms < self.last_ms;
        let open = match landing {
            Lands::First(k) => k,
            Lands::Ahead(_) => at_unix_ms.div_euclid(SLOT_MS),
            Lands::Newest => self
                .open_k
                .unwrap_or_else(|| at_unix_ms.div_euclid(SLOT_MS)),
            Lands::Back(by) => {
                for c in &mut self.covered {
                    c.0 -= by;
                }
                for p in &mut self.programs {
                    for s in &mut p.slots {
                        s.0 -= by;
                    }
                }
                at_unix_ms.div_euclid(SLOT_MS)
            }
        };
        if !clamped_back {
            self.last_ms = at_unix_ms;
        }
        if self.open_k != Some(open) {
            self.open_k = Some(open);
            let oldest = open - (HOUR_SLOTS - 1);
            self.covered.retain(|c| c.0 >= oldest);
            for p in &mut self.programs {
                p.slots.retain(|s| s.0 >= oldest);
            }
            self.programs.retain(|p| p.total() > 0);
        }
        open
    }

    /// Record a sweep window of `window_ms` that formed a rate at `at_unix_ms`, and what each
    /// program wrote in it ([`Writer::bytes`]).
    pub fn note(&mut self, at_unix_ms: i64, window_ms: u64, writers: &[Writer]) {
        let k = self.at(at_unix_ms);
        self.notes += 1;
        match self.covered.back_mut() {
            Some(c) if c.0 == k => c.1 = c.1.saturating_add(window_ms),
            _ => self.covered.push_back((k, window_ms)),
        }
        for w in writers.iter().filter(|w| w.bytes > 0) {
            match self
                .programs
                .iter_mut()
                .find(|p| p.scope == w.scope && p.name == w.name)
            {
                Some(p) => {
                    p.add(k, w.bytes, w.n);
                    p.noted = self.notes;
                }
                None => self.admit(k, w),
            }
        }
    }

    /// Take in a program not kept yet. At [`WRITE_PROGRAMS`] it takes the place of the smallest
    /// hour outside its scope's first [`TOP_WRITERS`] (what the deck can show) that wrote nothing
    /// in this window, whatever the newcomer wrote: a steady writer that starts small then climbs
    /// by what it adds, and one-off newcomers take each other's places, never that of a program
    /// writing in the same window. Where no such hour is kept, it takes the place of the smallest
    /// hour that wrote nothing in this window only when it wrote more than that hour and is among
    /// its own scope's first.
    fn admit(&mut self, k: i64, w: &Writer) {
        if self.programs.len() >= WRITE_PROGRAMS {
            let totals: Vec<u64> = self.programs.iter().map(Program::total).collect();
            let mut order: Vec<usize> = (0..totals.len()).collect();
            order.sort_by(|&a, &b| totals[b].cmp(&totals[a]));
            let idle = |i: usize| self.programs[i].noted != self.notes;
            let mut ranks: Vec<(ProcScope, usize)> = Vec::new();
            let mut spare = None;
            for &i in &order {
                if rank_in(&mut ranks, self.programs[i].scope) > TOP_WRITERS && idle(i) {
                    spare = Some(i);
                }
            }
            let victim = match spare {
                Some(i) => i,
                None => {
                    let Some(smallest) = order.iter().rev().copied().find(|&i| idle(i)) else {
                        return;
                    };
                    let ahead = (0..totals.len())
                        .filter(|&i| self.programs[i].scope == w.scope && totals[i] >= w.bytes)
                        .count();
                    if ahead >= TOP_WRITERS || w.bytes <= totals[smallest] {
                        return;
                    }
                    smallest
                }
            };
            self.programs.swap_remove(victim);
        }
        self.programs.push(Program {
            name: w.name.clone(),
            scope: w.scope,
            slots: vec![(k, w.bytes, w.n)],
            noted: self.notes,
        });
    }

    /// Fold the programs of tabs no longer in `live` into aterm's, so the hour of all tabs keeps
    /// what they wrote.
    pub fn fold_closed(&mut self, live: &HashSet<u64>) {
        let is_closed = |p: &Program| matches!(p.scope, ProcScope::Tab(k) if !live.contains(&k));
        if !self.programs.iter().any(is_closed) {
            return;
        }
        let (closed, kept): (Vec<Program>, Vec<Program>) = std::mem::take(&mut self.programs)
            .into_iter()
            .partition(is_closed);
        self.programs = kept;
        for c in closed {
            match self
                .programs
                .iter_mut()
                .find(|p| p.scope == ProcScope::Aterm && p.name == c.name)
            {
                Some(p) => {
                    for (k, bytes, n) in c.slots {
                        p.add(k, bytes, n);
                    }
                    p.noted = p.noted.max(c.noted);
                }
                None => self.programs.push(Program {
                    scope: ProcScope::Aterm,
                    ..c
                }),
            }
        }
    }

    /// The programs that wrote most in the hour, busiest first, at most [`TOP_WRITERS`] in each
    /// scope, so each of the deck's filters (the Mac, all tabs, this tab) finds its own first
    /// [`TOP_WRITERS`] among them.
    #[must_use]
    pub fn top(&self) -> Vec<ProgramHour> {
        let mut all: Vec<ProgramHour> = self
            .programs
            .iter()
            .map(|p| ProgramHour {
                name: p.name.clone(),
                scope: p.scope,
                bytes: p.total(),
                n_max: p.slots.iter().map(|s| s.2).max().unwrap_or(0),
            })
            .filter(|p| p.bytes > 0)
            .collect();
        all.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
        top_per_scope(all, |p| p.scope)
    }

    /// Seconds of the hour the sweep windows behind it covered.
    #[must_use]
    pub fn covered_s(&self) -> f64 {
        self.covered
            .iter()
            .fold(0u64, |sum, c| sum.saturating_add(c.1)) as f64
            / 1000.0
    }
}

/// `scope`'s rank so far in `ranks`, counting this one.
fn rank_in(ranks: &mut Vec<(ProcScope, usize)>, scope: ProcScope) -> usize {
    match ranks.iter_mut().find(|r| r.0 == scope) {
        Some(r) => {
            r.1 += 1;
            r.1
        }
        None => {
            ranks.push((scope, 1));
            1
        }
    }
}

/// `items`, busiest first, less each past the [`TOP_WRITERS`]th of its scope.
pub(crate) fn top_per_scope<T>(
    items: impl IntoIterator<Item = T>,
    scope: impl Fn(&T) -> ProcScope,
) -> Vec<T> {
    let mut ranks = Vec::new();
    items
        .into_iter()
        .filter(|it| rank_in(&mut ranks, scope(it)) <= TOP_WRITERS)
        .collect()
}

#[cfg(test)]
#[path = "writes_tests.rs"]
mod tests;
