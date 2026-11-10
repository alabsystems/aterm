// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The heat words of a resource coalition's usage record
//! (`docs/DESIGN-stats-deck-instruments-2026-11-06.md` §4, M-COAL): CPU energy, the Super
//! (performance) cores' share of CPU time, cycles and instructions, the Neural Engine's time and
//! energy, and the GPU's energy; and the record half of the self-check that recognises them on the
//! running kernel.
//!
//! The words are xnu-private (`struct coalition_resource_usage`, `bsd/sys/coalition.h`) with no SDK
//! header to hold them to, so the layout is recognised by invariants, never trusted:
//!
//! - a record decodes ([`decode`]) only with the QoS count [`crate::coalitions`] reads, every word
//!   read written, and its Super cycles and instructions within their wholes;
//! - the coalition read's own copy loop decodes each record as the kernel copies it out and sums
//!   them there ([`CoalHeat`], carried by [`crate::coalitions::CoalitionRead::heat`]): one record
//!   that does not decode refuses the read's heat;
//! - the change between two reads, and the judgement of it against the chip and the machine (the
//!   busy core time, the clock range, instructions per cycle, watts), are the model's
//!   (`aterm_vitals::heat::change`, `aterm_vitals::heat::judge`), so one engine judges them.
//!
//! A reading that fails any of them reads as unread, never as a shifted value.
//!
//! The record's times are three accounts, not one: `cpu_time` (word 3), `cpu_ptime` (word 22) and
//! the effective-QoS times (words 24–30). On xnu-13432 (macOS 27.0.1) a coalition's lifetime
//! `cpu_ptime` read above its `cpu_time`, and the QoS times sum above both, so Super time is never
//! held within either whole; `cpu_time` undercounts the busy cores (0.926 p50 in N1), and the
//! QoS-time sum matches them (0.995).
//!
//! [`HeatRead`] and [`moved`] give the change by coalition id, as a pass after the read: the
//! oracle the copy loop's folded form is held to, and N1's measured separate pass
//! (`examples/coal_clocks.rs`), never the reading's path.

use crate::coalitions::{
    CPU_TIME, EQOS, EQOS_LEN, QOS_CLASSES, RecordRead, UNWRITTEN, USAGE_WORDS,
};

/// CPU energy, nanojoules (`energy`).
pub const ENERGY: usize = 11;
/// CPU time on Super cores, Mach ticks (`cpu_ptime`).
pub const P_TIME: usize = 22;
/// Instructions retired (`cpu_instructions`).
pub const INSTRUCTIONS: usize = 31;
/// Cycles (`cpu_cycles`).
pub const CYCLES: usize = 32;
/// Instructions retired on Super cores (`cpu_pinstructions`).
pub const P_INSTRUCTIONS: usize = 35;
/// Cycles on Super cores (`cpu_pcycles`).
pub const P_CYCLES: usize = 36;
/// Neural Engine time, Mach ticks (`ane_mach_time`).
pub const ANE_TIME: usize = 38;
/// Neural Engine energy, nanojoules (`ane_energy_nj`).
pub const ANE_ENERGY: usize = 39;
/// GPU energy, nanojoules (`gpu_energy_nj`).
pub const GPU_ENERGY: usize = 41;

/// Every single word [`decode`] reads, beside the QoS times.
pub const WORDS: [usize; 10] = [
    CPU_TIME,
    P_TIME,
    ENERGY,
    INSTRUCTIONS,
    CYCLES,
    P_INSTRUCTIONS,
    P_CYCLES,
    ANE_TIME,
    ANE_ENERGY,
    GPU_ENERGY,
];

/// How many heat counters a record gives ([`Heat::words`]); the model's `heat::COUNTERS`.
pub const COUNTERS: usize = 11;

/// One coalition's heat counters, as one record gave them (or a sum, or a change, of them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Heat {
    /// CPU time, Mach ticks (`cpu_time`).
    pub cpu_ticks: u64,
    /// CPU time by effective QoS, summed, Mach ticks.
    pub qos_ticks: u64,
    /// CPU time on Super cores, Mach ticks.
    pub p_ticks: u64,
    /// CPU energy, nanojoules.
    pub energy_nj: u64,
    /// Instructions retired.
    pub instructions: u64,
    /// Cycles.
    pub cycles: u64,
    /// Instructions retired on Super cores.
    pub p_instructions: u64,
    /// Cycles on Super cores.
    pub p_cycles: u64,
    /// Neural Engine time, Mach ticks.
    pub ane_ticks: u64,
    /// Neural Engine energy, nanojoules.
    pub ane_energy_nj: u64,
    /// GPU energy, nanojoules.
    pub gpu_energy_nj: u64,
}

impl Heat {
    /// The counters as words, in field order: the order `aterm_vitals::heat::Counters::from_words`
    /// reads (held by this crate's tests against the model's names).
    #[must_use]
    pub const fn words(self) -> [u64; COUNTERS] {
        [
            self.cpu_ticks,
            self.qos_ticks,
            self.p_ticks,
            self.energy_nj,
            self.instructions,
            self.cycles,
            self.p_instructions,
            self.p_cycles,
            self.ane_ticks,
            self.ane_energy_nj,
            self.gpu_energy_nj,
        ]
    }

    /// The counters back from [`Self::words`].
    #[must_use]
    pub const fn of_words(c: [u64; COUNTERS]) -> Self {
        Self {
            cpu_ticks: c[0],
            qos_ticks: c[1],
            p_ticks: c[2],
            energy_nj: c[3],
            instructions: c[4],
            cycles: c[5],
            p_instructions: c[6],
            p_cycles: c[7],
            ane_ticks: c[8],
            ane_energy_nj: c[9],
            gpu_energy_nj: c[10],
        }
    }

    /// Each counter's change from `then`; `None` where one ran backwards.
    #[must_use]
    pub fn since(self, then: Self) -> Option<Self> {
        let (now, then) = (self.words(), then.words());
        let mut out = [0; COUNTERS];
        for (o, (n, t)) in out.iter_mut().zip(now.iter().zip(then)) {
            *o = n.checked_sub(t)?;
        }
        Some(Self::of_words(out))
    }

    /// Each counter's sum with `other`'s, saturating.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        let (a, b) = (self.words(), other.words());
        let mut out = [0; COUNTERS];
        for (o, (x, y)) in out.iter_mut().zip(a.iter().zip(b)) {
            *o = x.saturating_add(y);
        }
        Self::of_words(out)
    }

    /// Whether the Super cycles and instructions lie within their wholes (one account each).
    #[must_use]
    pub const fn super_within_whole(self) -> bool {
        self.p_cycles <= self.cycles && self.p_instructions <= self.instructions
    }
}

/// A record's heat counters.
///
/// # Errors
/// `layout` for another QoS count (another record layout), `unwritten` where the record ends
/// before a word read, `super_over_whole` where Super cycles or instructions exceed their whole.
pub fn decode(raw: &[u64; USAGE_WORDS]) -> Result<Heat, &'static str> {
    if raw[EQOS_LEN] != QOS_CLASSES as u64 {
        return Err("layout");
    }
    let qos = &raw[EQOS..EQOS + QOS_CLASSES];
    if WORDS.iter().any(|&w| raw[w] == UNWRITTEN) || qos.contains(&UNWRITTEN) {
        return Err("unwritten");
    }
    let heat = Heat {
        cpu_ticks: raw[CPU_TIME],
        qos_ticks: qos.iter().fold(0u64, |a, t| a.saturating_add(*t)),
        p_ticks: raw[P_TIME],
        energy_nj: raw[ENERGY],
        instructions: raw[INSTRUCTIONS],
        cycles: raw[CYCLES],
        p_instructions: raw[P_INSTRUCTIONS],
        p_cycles: raw[P_CYCLES],
        ane_ticks: raw[ANE_TIME],
        ane_energy_nj: raw[ANE_ENERGY],
        gpu_energy_nj: raw[GPU_ENERGY],
    };
    if heat.super_within_whole() {
        Ok(heat)
    } else {
        Err("super_over_whole")
    }
}

/// One reading's decoded heat counters, by coalition id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeatRead {
    /// The highest id the kernel listed.
    pub newest: Option<u64>,
    /// Whether the list filled its buffer.
    pub truncated: bool,
    /// Each decoded coalition's counters, ascending by id.
    pub rows: Vec<(u64, Heat)>,
    /// The first refusal of a record that did not decode, and how many did not.
    pub refused: Option<(&'static str, u32)>,
}

impl HeatRead {
    /// The heat counters of every record of `read`.
    #[must_use]
    pub fn of(read: &RecordRead) -> Self {
        let mut out = Self::default();
        out.refill(read);
        out
    }

    /// Replaces this read with `read`'s heat counters, keeping its rows' allocation.
    pub fn refill(&mut self, read: &RecordRead) {
        self.newest = read.newest;
        self.truncated = read.truncated;
        self.refused = None;
        self.rows.clear();
        for (id, raw) in &read.records {
            match decode(raw) {
                Ok(heat) => self.rows.push((*id, heat)),
                Err(why) => self.refused.get_or_insert((why, 0)).1 += 1,
            }
        }
        if !self.rows.is_sorted_by_key(|r| r.0) {
            self.rows.sort_unstable_by_key(|r| r.0);
        }
        self.rows.dedup_by_key(|r| r.0);
    }
}

/// The counters' change between two readings ([`moved`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Moved {
    /// The summed change.
    pub sum: Heat,
    /// Coalitions both reads decoded.
    pub matched: u32,
    /// Coalitions born since the earlier read (above its newest id), counted whole.
    pub born: u32,
    /// Coalitions at or below the earlier read's newest id that it did not decode: no base, left
    /// out.
    pub unbased: u32,
    /// Matched coalitions whose Super time moved more than their `cpu_time`.
    pub p_over: u32,
}

/// The heat counters' change from `then` to `now`: each coalition both decoded, plus each born
/// since `then` whole. One gone since took its last change with it, and one with no base is left
/// out ([`Moved::unbased`]); both leave the clocks' ratios whole.
///
/// # Errors
/// `truncated` across a cut list, `refused_record` where a record of either did not decode,
/// `backwards` where a counter of one coalition ran backwards, `empty` where `now` decoded none.
pub fn moved(then: &HeatRead, now: &HeatRead) -> Result<Moved, &'static str> {
    if then.truncated || now.truncated {
        return Err("truncated");
    }
    if then.refused.is_some() || now.refused.is_some() {
        return Err("refused_record");
    }
    if now.rows.is_empty() {
        return Err("empty");
    }
    let mut out = Moved::default();
    let mut earlier = then.rows.iter().peekable();
    for &(id, heat) in &now.rows {
        while earlier.next_if(|(e, _)| *e < id).is_some() {}
        match earlier.peek() {
            Some(&&(e, base)) if e == id => {
                let d = heat.since(base).ok_or("backwards")?;
                out.p_over += u32::from(d.p_ticks > d.cpu_ticks);
                out.sum = out.sum.plus(d);
                out.matched += 1;
            }
            _ if then.newest.is_none_or(|n| id > n) => {
                out.sum = out.sum.plus(heat);
                out.born += 1;
            }
            _ => out.unbased += 1,
        }
    }
    Ok(out)
}

/// One read's heat counters as its copy loop leaves them: each record decoded where the kernel
/// copied it out, kept by id in the kernel's order and summed as it goes (M-COAL's `totals24`:
/// the start counter's 24-byte task rows stay as they are, and no pass after the read runs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoalHeat {
    /// The Mach timebase `(numer, denom)` the read's times tick at.
    pub timebase: (u32, u32),
    /// Each record that decoded: its coalition id and [`Heat::words`], in the kernel's order.
    pub by_id: Vec<(u64, [u64; COUNTERS])>,
    /// The words summed; the first refusal where a record did not decode, or `overflow` where a
    /// sum would pass `u64` (bounded by the machine's counts since boot, it has not been seen),
    /// after which the loop keeps nothing more.
    pub total: Result<[u64; COUNTERS], &'static str>,
}

impl Default for CoalHeat {
    fn default() -> Self {
        Self {
            timebase: (1, 1),
            by_id: Vec::new(),
            total: Err("not_read"),
        }
    }
}

impl CoalHeat {
    /// An empty read at `timebase`, room kept for `records`.
    #[must_use]
    pub fn with_capacity(records: usize, timebase: (u32, u32)) -> Self {
        Self {
            timebase,
            by_id: Vec::with_capacity(records),
            total: Ok([0; COUNTERS]),
        }
    }

    /// Notes coalition `id`'s record `raw`, as the copy loop copies it out: decoded, kept and
    /// summed, or, where it does not decode or its sum would pass `u64` (`overflow`), the read's
    /// heat refused with why.
    pub fn note(&mut self, id: u64, raw: &[u64; USAGE_WORDS]) {
        let Ok(total) = self.total else {
            return;
        };
        let summed = decode(raw).and_then(|heat| {
            let words = heat.words();
            let mut sum = total;
            for (t, w) in sum.iter_mut().zip(words) {
                *t = t.checked_add(w).ok_or("overflow")?;
            }
            Ok((sum, words))
        });
        match summed {
            Ok((sum, words)) => {
                self.total = Ok(sum);
                self.by_id.push((id, words));
            }
            Err(why) => {
                self.total = Err(why);
                self.by_id.clear();
            }
        }
    }
}

#[cfg(test)]
#[path = "coalition_heat_tests.rs"]
mod tests;
