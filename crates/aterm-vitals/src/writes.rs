// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The last hour of bytes written per program and scope ([`WriteHour`]), with the time its
//! readings covered: what the deck's Disk row names as its top writer ("rustc writes 68 GB/h")
//! and `sys disk` lists as `program` rows.
//!
//! A program is the processes of one name in one scope (aterm, a tab, the rest of the Mac), as
//! [`Writer`] groups them each sweep. The figure is a floor: the counter
//! (`ri_diskio_byteswritten`) is the I/O a process issued itself, so dirty pages the kernel
//! flushes later are not its, and what a process writes after its last sweep is lost unless a
//! sweep reads it as a zombie. No sweep runs while no deck is shown, so an hour figure is a rate
//! over the time the readings covered ([`ProgramHour::per_hour`]), never an hourly total.

use std::collections::{HashSet, VecDeque};

use crate::model::{ProcScope, TOP_WRITERS, Writer};
use crate::ring::CLAMP_BACK_S;

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
        (covered_s >= MIN_COVERED_S).then(|| self.bytes as f64 * 3600.0 / covered_s)
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
/// as [`crate::history::FreeHistory`] keys its own: a reading up to [`CLAMP_BACK_S`] behind the
/// newest lands in the newest slot, and a longer step back moves the whole hour back with it.
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
        let k = at_unix_ms.div_euclid(SLOT_MS);
        let open = match self.open_k {
            None => k,
            Some(open) if k > open => k,
            Some(open)
                if k < open
                    && i128::from(self.last_ms) - i128::from(at_unix_ms)
                        > i128::from(CLAMP_BACK_S * 1000) =>
            {
                let by = open - k;
                for c in &mut self.covered {
                    c.0 -= by;
                }
                for p in &mut self.programs {
                    for s in &mut p.slots {
                        s.0 -= by;
                    }
                }
                k
            }
            Some(open) => open,
        };
        self.last_ms = at_unix_ms;
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
mod tests {
    use super::*;

    /// 2026-10-23 12:00:05 UTC, five seconds into a minute.
    const T0: i64 = 1_792_756_805_000;
    const MB: u64 = 1_000_000;

    fn w(name: &str, scope: ProcScope, bytes: u64, n: u32) -> Writer {
        Writer {
            name: name.into(),
            bytes_per_s: 0.0,
            scope,
            n,
            bytes,
        }
    }

    fn names(top: &[ProgramHour]) -> Vec<(&str, ProcScope, u64)> {
        top.iter()
            .map(|p| (p.name.as_str(), p.scope, p.bytes))
            .collect()
    }

    #[test]
    fn the_hour_scales_bytes_over_covered_time() {
        let mut h = WriteHour::new();
        for i in 0..10 {
            h.note(
                T0 + i * 30_000,
                30_000,
                &[w("rustc", ProcScope::Tab(3), 1_000 * MB, 9)],
            );
        }
        assert!((h.covered_s() - 300.0).abs() < 1e-9, "{}", h.covered_s());
        let top = h.top();
        assert_eq!(names(&top), [("rustc", ProcScope::Tab(3), 10_000 * MB)]);
        assert_eq!(top[0].n_max, 9);
        let per_hour = top[0].per_hour(h.covered_s()).expect("five minutes read");
        assert!((per_hour - 120e9).abs() < 1.0, "{per_hour}");
        assert_eq!(
            top[0].per_hour(MIN_COVERED_S - 0.5),
            None,
            "too little read"
        );
    }

    #[test]
    fn minutes_older_than_an_hour_drop() {
        let mut h = WriteHour::new();
        h.note(T0, 1_000, &[w("cp", ProcScope::Other, 5 * MB, 1)]);
        h.note(
            T0 + 59 * SLOT_MS,
            1_000,
            &[w("rustc", ProcScope::Aterm, 7 * MB, 2)],
        );
        assert_eq!(
            names(&h.top()),
            [
                ("rustc", ProcScope::Aterm, 7 * MB),
                ("cp", ProcScope::Other, 5 * MB)
            ]
        );
        assert!((h.covered_s() - 2.0).abs() < 1e-9);
        let _ = h.at(T0 + 60 * SLOT_MS);
        assert_eq!(names(&h.top()), [("rustc", ProcScope::Aterm, 7 * MB)]);
        assert!((h.covered_s() - 1.0).abs() < 1e-9, "the first minute went");
        let _ = h.at(T0 + 119 * SLOT_MS);
        assert_eq!(names(&h.top()), [], "nothing written in the hour");
        assert_eq!(h.covered_s(), 0.0);
    }

    /// A reading that moves the hour without a window (the model's gap, or no sweep at all)
    /// adds neither coverage nor bytes.
    #[test]
    fn a_reading_with_no_window_adds_neither_coverage_nor_bytes() {
        let mut h = WriteHour::new();
        h.note(T0, 2_000, &[w("cp", ProcScope::Other, 5 * MB, 1)]);
        for i in 1..30 {
            let _ = h.at(T0 + i * 60_000);
        }
        assert!((h.covered_s() - 2.0).abs() < 1e-9);
        assert_eq!(names(&h.top()), [("cp", ProcScope::Other, 5 * MB)]);
    }

    #[test]
    fn a_clock_step_back_within_the_clamp_lands_in_the_open_minute() {
        let mut h = WriteHour::new();
        let open = h.at(T0);
        assert_eq!(open, T0.div_euclid(SLOT_MS));
        assert_eq!(
            h.at(T0 - 30_000),
            open,
            "30 s back, into the minute before: the open minute"
        );
        h.note(T0 - 30_000, 1_000, &[w("cp", ProcScope::Other, MB, 1)]);
        assert_eq!(h.covered.len(), 1, "{:?}", h.covered);
        assert_eq!(h.covered[0].0, open);
        // Past the clamp the whole hour moves back: the newest minute is the reading's.
        let back = h.at(T0 - 30_000 - 2 * SLOT_MS);
        assert_eq!(back, (T0 - 30_000 - 2 * SLOT_MS).div_euclid(SLOT_MS));
        assert_eq!(h.covered[0].0, back, "moved with the clock");
        assert_eq!(names(&h.top()), [("cp", ProcScope::Other, MB)]);
        assert!((h.covered_s() - 1.0).abs() < 1e-9);
    }

    /// `name` is among the programs the hour keeps.
    fn kept(h: &WriteHour, name: &str) -> bool {
        h.programs.iter().any(|p| p.name == name)
    }

    #[test]
    fn the_cap_evicts_the_smallest_hour() {
        let mut h = WriteHour::new();
        let full: Vec<Writer> = (1..=WRITE_PROGRAMS as u64)
            .map(|i| w(&format!("p{i}"), ProcScope::Other, i * MB, 1))
            .collect();
        h.note(T0, 1_000, &full);
        assert_eq!(h.programs.len(), WRITE_PROGRAMS);
        h.note(
            T0 + 1_000,
            1_000,
            &[w("big", ProcScope::Other, 100 * MB, 1)],
        );
        assert_eq!(h.programs.len(), WRITE_PROGRAMS);
        assert!(!kept(&h, "p1"), "the smallest went");
        assert!(kept(&h, "big"));
        // A newcomer smaller than every hour kept still comes in, in the smallest's place.
        h.note(T0 + 2_000, 1_000, &[w("tiny", ProcScope::Other, MB / 2, 1)]);
        assert!(kept(&h, "tiny"));
        assert!(!kept(&h, "p2"));
        // A tab's first program comes in however small, and the newcomer before it, now the
        // smallest outside its scope's first four, makes room.
        h.note(T0 + 3_000, 1_000, &[w("zsh", ProcScope::Tab(5), MB / 4, 1)]);
        assert!(kept(&h, "zsh"));
        assert!(!kept(&h, "tiny"));
        assert!(kept(&h, "p3"));
        assert_eq!(h.programs.len(), WRITE_PROGRAMS);
        // NEGATIVE CONTROL: no scope's first four makes room, though zsh's tab hour is the
        // smallest kept: the smallest of the crowded scope goes.
        h.note(T0 + 4_000, 1_000, &[w("cc", ProcScope::Other, MB / 8, 1)]);
        assert!(kept(&h, "cc"));
        assert!(!kept(&h, "p3"));
        let tab: Vec<_> = h
            .top()
            .into_iter()
            .filter(|p| p.scope == ProcScope::Tab(5))
            .collect();
        assert_eq!(names(&tab), [("zsh", ProcScope::Tab(5), MB / 4)]);
        let other: Vec<_> = h
            .top()
            .into_iter()
            .filter(|p| p.scope == ProcScope::Other)
            .map(|p| p.name)
            .collect();
        assert_eq!(other, ["big", "p64", "p63", "p62"]);
    }

    /// A STEADY NEWCOMER CLIMBS A FULL HOUR: a program that writes 1 MB a window into an hour
    /// full of programs that each wrote more than 10 MB, with a one-off newcomer after it in
    /// every window, comes in and climbs by what it adds until it leads its scope. NEGATIVE
    /// CONTROL: the one-off newcomers take each other's places, so of the hours kept before it
    /// only the two smallest went.
    #[test]
    fn a_steady_newcomer_climbs_a_full_hour() {
        let mut h = WriteHour::new();
        let full: Vec<Writer> = (1..=WRITE_PROGRAMS as u64)
            .map(|i| w(&format!("p{i}"), ProcScope::Other, (10 + i) * MB, 1))
            .collect();
        h.note(T0, 1_000, &full);
        for i in 1..=120 {
            h.note(
                T0 + i * 1_000,
                1_000,
                &[
                    w("steady", ProcScope::Other, MB, 1),
                    w(&format!("once{i}"), ProcScope::Other, 1_000, 1),
                ],
            );
        }
        let top = h.top();
        assert_eq!(
            (top[0].name.as_str(), top[0].bytes),
            ("steady", 120 * MB),
            "{:?}",
            names(&top)
        );
        assert!(!kept(&h, "p1") && !kept(&h, "p2"));
        assert!(
            (3..=WRITE_PROGRAMS).all(|i| kept(&h, &format!("p{i}"))),
            "{:?}",
            h.programs.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        assert!(kept(&h, "once120") && !kept(&h, "once119"));
    }

    /// WHERE EVERY HOUR KEPT LEADS ITS SCOPE (sixteen tabs of four), a newcomer comes in only
    /// among its own scope's first four and past the smallest hour.
    #[test]
    fn with_no_spare_a_newcomer_must_lead_its_scope_and_outwrite_the_smallest() {
        let mut h = WriteHour::new();
        let full: Vec<Writer> = (0..WRITE_PROGRAMS as u64)
            .map(|i| w(&format!("p{i}"), ProcScope::Tab(i / 4), (10 + i) * MB, 1))
            .collect();
        h.note(T0, 1_000, &full);
        h.note(
            T0 + 1_000,
            1_000,
            &[w("fifth", ProcScope::Tab(15), 50 * MB, 1)],
        );
        assert!(
            !kept(&h, "fifth"),
            "the fifth of a full tab, though past the smallest hour"
        );
        h.note(
            T0 + 2_000,
            1_000,
            &[w("small", ProcScope::Tab(99), 5 * MB, 1)],
        );
        assert!(!kept(&h, "small"), "under the smallest hour");
        h.note(
            T0 + 3_000,
            1_000,
            &[w("new", ProcScope::Tab(99), 11 * MB, 1)],
        );
        assert!(kept(&h, "new"));
        assert!(!kept(&h, "p0"), "the smallest went");
        assert_eq!(h.programs.len(), WRITE_PROGRAMS);
    }

    #[test]
    fn a_closed_tabs_writes_stay_in_all_tabs() {
        let mut h = WriteHour::new();
        h.note(
            T0,
            1_000,
            &[
                w("rustc", ProcScope::Tab(3), 5_000 * MB, 9),
                w("rustc", ProcScope::Aterm, 1_000 * MB, 2),
                w("git", ProcScope::Tab(4), 3 * MB, 1),
            ],
        );
        h.note(
            T0 + SLOT_MS,
            1_000,
            &[w("rustc", ProcScope::Tab(3), 2_000 * MB, 4)],
        );
        let all_tabs = |h: &WriteHour| -> u64 {
            h.top()
                .iter()
                .filter(|p| p.scope != ProcScope::Other)
                .map(|p| p.bytes)
                .sum()
        };
        let before = all_tabs(&h);
        h.fold_closed(&HashSet::from([4]));
        assert_eq!(
            all_tabs(&h),
            before,
            "all tabs keeps the closed tab's bytes"
        );
        let top = h.top();
        assert_eq!(
            names(&top),
            [
                ("rustc", ProcScope::Aterm, 8_000 * MB),
                ("git", ProcScope::Tab(4), 3 * MB)
            ]
        );
        assert_eq!(top[0].n_max, 9, "the most at once of either");
        assert!(top.iter().all(|p| p.scope != ProcScope::Tab(3)));
    }

    #[test]
    fn each_scope_filter_gets_its_exact_top() {
        let mut h = WriteHour::new();
        let mut ws = Vec::new();
        for (i, name) in ["rustc", "cc", "ld", "cargo", "git", "zsh"]
            .iter()
            .enumerate()
        {
            ws.push(w(name, ProcScope::Tab(3), (60 - i as u64) * MB, 1));
            ws.push(w(name, ProcScope::Other, (100 - i as u64) * MB, 1));
        }
        ws.push(w("aterm", ProcScope::Aterm, 2 * MB, 1));
        ws.push(w("helper", ProcScope::Aterm, MB, 1));
        h.note(T0, 1_000, &ws);
        let mut every: Vec<_> = ws
            .iter()
            .map(|w| (w.name.as_str(), w.scope, w.bytes))
            .collect();
        every.sort_by_key(|p| std::cmp::Reverse(p.2));
        let top = h.top();
        let kept = names(&top);
        /// A deck filter: its label and the scopes it shows.
        type Filter = (&'static str, fn(ProcScope) -> bool);
        let filters: [Filter; 3] = [
            ("Mac", |_| true),
            ("all tabs", |s| s != ProcScope::Other),
            ("this tab", |s| s == ProcScope::Tab(3)),
        ];
        for (label, keep) in filters {
            let want: Vec<_> = every
                .iter()
                .copied()
                .filter(|p| keep(p.1))
                .take(TOP_WRITERS)
                .collect();
            let got: Vec<_> = kept
                .iter()
                .copied()
                .filter(|p| keep(p.1))
                .take(TOP_WRITERS)
                .collect();
            assert_eq!(got, want, "{label}");
        }
        // NEGATIVE CONTROL: the Mac's first four alone would leave this tab's empty.
        let mac_only: Vec<_> = every.iter().take(TOP_WRITERS).collect();
        assert!(mac_only.iter().all(|p| p.1 == ProcScope::Other));
        assert_eq!(
            kept.iter().filter(|p| p.1 == ProcScope::Tab(3)).count(),
            TOP_WRITERS
        );
    }
}
