// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The heat words of a resource coalition's usage record
//! (`docs/DESIGN-stats-deck-instruments-2026-11-06.md` §4, M-COAL): CPU energy, the Super
//! (performance) cores' share of CPU time, cycles and instructions, the Neural Engine's time and
//! energy, and the GPU's energy; and the self-check that recognises them on the running kernel.
//!
//! The words are xnu-private (`struct coalition_resource_usage`, `bsd/sys/coalition.h`) with no SDK
//! header to hold them to, so the layout is recognised by invariants, never trusted:
//!
//! - a record decodes ([`decode`]) only with the QoS count [`crate::coalitions`] reads, every word
//!   read written, and its Super cycles and instructions within their wholes;
//! - a reading's change ([`moved`]) is summed over the coalitions both reads answered, by id, plus
//!   those born since; a counter of one that ran backwards refuses the reading;
//! - a reading is judged ([`judge`]): Super cycles and instructions within their wholes again; its
//!   CPU time and Super time against the busy core time the per-core ticks gave ([`Busy`]); every
//!   core's cycles ÷ time and the Super cores' within the chip's clock range; instructions per
//!   cycle and CPU watts per busy core; GPU and Neural Engine watts and the Neural Engine's busy
//!   share each in a plausible range.
//!
//! A reading that fails any of them reads as unread, never as a shifted value.
//!
//! The record's times are three accounts, not one: `cpu_time` (word 3), `cpu_ptime` (word 22) and
//! the effective-QoS times (words 24–30). On xnu-13432 (macOS 27.0.1) a coalition's lifetime
//! `cpu_ptime` read above its `cpu_time`, and the QoS times sum above both, so Super time is never
//! held within either whole, and the Efficiency clock ((cycles − Super cycles) ÷ (whole − Super
//! time)) is given over both wholes ([`Judged::e_mhz`], [`Judged::e_qos_mhz`]), each refused alone
//! past its table's range without refusing the reading.

use crate::coalitions::{
    CPU_TIME, EQOS, EQOS_LEN, QOS_CLASSES, RecordRead, UNWRITTEN, USAGE_WORDS,
};
use crate::procs::ticks_to_ns;

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

/// Thread time under which a reading's clocks, IPC and CPU watts are not judged or given: a few
/// ticks' rounding would decide them.
pub const MIN_CLOCK_NS: u64 = 10_000_000;

/// How far under a table's bottom a derived clock may read and still be the clock.
pub const BELOW_BOTTOM: f64 = 0.5;

/// How far over a table's top a derived clock may read and still be the clock.
pub const OVER_TOP: f64 = 1.02;

/// Instructions per cycle no core retires at or past.
pub const MAX_IPC: f64 = 16.0;

/// Instructions per cycle under which the words are not instructions and cycles.
pub const MIN_IPC: f64 = 0.01;

/// CPU watts per busy core (nanojoules per nanosecond of CPU time) a reading may give.
pub const CPU_W_PER_CORE: (f64, f64) = (0.01, 25.0);

/// GPU watts a reading may give.
pub const MAX_GPU_W: f64 = 200.0;

/// Neural Engine watts a reading may give.
pub const MAX_ANE_W: f64 = 50.0;

/// Neural Engine busy share (its time ÷ the window) a reading may give: two engines, and slack.
pub const MAX_ANE_BUSY: f64 = 2.1;

/// How far a reading's CPU time, and its Super time, may stray from the busy core time the
/// per-core ticks gave ([`Busy`]): a time word that moved to a neighbour reads near 0 or many
/// times the busy time.
pub const BUSY_RATIO: (f64, f64) = (0.5, 1.5);

const COUNTERS: usize = 11;

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
    fn counters(self) -> [u64; COUNTERS] {
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

    fn of_counters(c: [u64; COUNTERS]) -> Self {
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
        let (now, then) = (self.counters(), then.counters());
        let mut out = [0; COUNTERS];
        for (o, (n, t)) in out.iter_mut().zip(now.iter().zip(then)) {
            *o = n.checked_sub(t)?;
        }
        Some(Self::of_counters(out))
    }

    /// Each counter's sum with `other`'s, saturating.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        let (a, b) = (self.counters(), other.counters());
        let mut out = [0; COUNTERS];
        for (o, (x, y)) in out.iter_mut().zip(a.iter().zip(b)) {
            *o = x.saturating_add(y);
        }
        Self::of_counters(out)
    }

    /// Whether the Super cycles and instructions lie within their wholes (one account each).
    #[must_use]
    pub fn super_within_whole(self) -> bool {
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

/// The chip's clock range: each core kind's DVFS table bottom and top, MHz.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockRange {
    /// Super cores' bottom and top.
    pub p_mhz: (f64, f64),
    /// Efficiency cores' bottom and top.
    pub e_mhz: (f64, f64),
}

/// The busy core time the machine's per-core ticks gave over a reading's window, nanoseconds of
/// core: every core's and the Super cores'.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Busy {
    /// Every core's busy time.
    pub all_ns: u64,
    /// The Super cores' busy time.
    pub p_ns: u64,
}

/// A judged reading ([`judge`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Judged {
    /// CPU time (`cpu_time`), seconds of core.
    pub cpu_s: f64,
    /// CPU time by effective QoS, seconds of core.
    pub qos_s: f64,
    /// CPU time on Super cores, seconds of core.
    pub p_s: f64,
    /// Every core's cycles ÷ `cpu_time`, MHz; `None` under [`MIN_CLOCK_NS`].
    pub mhz: Option<f64>,
    /// Super cycles ÷ Super time, MHz; `None` under [`MIN_CLOCK_NS`].
    pub p_mhz: Option<f64>,
    /// (cycles − Super cycles) ÷ (`cpu_time` − Super time), MHz; `None` under [`MIN_CLOCK_NS`]
    /// or past the Efficiency table's range ([`Judged::e_refused`]).
    pub e_mhz: Option<f64>,
    /// Whether [`Judged::e_mhz`] read past its table's range and was refused.
    pub e_refused: bool,
    /// (cycles − Super cycles) ÷ (QoS time − Super time), MHz, refused as [`Judged::e_mhz`] is.
    pub e_qos_mhz: Option<f64>,
    /// Whether [`Judged::e_qos_mhz`] read past its table's range and was refused.
    pub e_qos_refused: bool,
    /// Instructions per cycle; `None` under [`MIN_CLOCK_NS`].
    pub ipc: Option<f64>,
    /// CPU watts over the window.
    pub cpu_w: f64,
    /// GPU watts over the window.
    pub gpu_w: f64,
    /// Neural Engine watts over the window.
    pub ane_w: f64,
    /// Neural Engine time ÷ the window.
    pub ane_busy: f64,
}

/// Judges a reading's change `m` over a `window_ns` window at the Mach `timebase` against the
/// chip's clock `range` and, where given, the `busy` core time of the same window (the module's
/// invariants).
///
/// # Errors
/// The first invariant that failed: `window`, `super_over_whole`, `busy_time`, `busy_p_time`,
/// `clock_range`, `p_clock_range`, `ipc`, `energy`, `gpu_energy`, `ane`.
pub fn judge(
    m: &Heat,
    window_ns: u64,
    (numer, denom): (u32, u32),
    range: &ClockRange,
    busy: Option<Busy>,
) -> Result<Judged, &'static str> {
    if window_ns == 0 {
        return Err("window");
    }
    if !m.super_within_whole() {
        return Err("super_over_whole");
    }
    let ns = |ticks: u64| ticks_to_ns(ticks, numer, denom);
    let (cpu_ns, qos_ns, p_ns) = (ns(m.cpu_ticks), ns(m.qos_ticks), ns(m.p_ticks));
    let strays = |time: u64, busy: u64| {
        (busy >= MIN_CLOCK_NS || time >= MIN_CLOCK_NS)
            && !(BUSY_RATIO.0..=BUSY_RATIO.1).contains(&(time as f64 / busy.max(1) as f64))
    };
    if let Some(busy) = busy {
        if strays(cpu_ns, busy.all_ns) {
            return Err("busy_time");
        }
        if strays(p_ns, busy.p_ns) {
            return Err("busy_p_time");
        }
    }
    let mhz = |cycles: u64, ns: u64| (ns >= MIN_CLOCK_NS).then(|| cycles as f64 * 1e3 / ns as f64);
    let within = |v: f64, (lo, hi): (f64, f64)| v >= lo * BELOW_BOTTOM && v <= hi * OVER_TOP;
    let whole = (
        range.p_mhz.0.min(range.e_mhz.0),
        range.p_mhz.1.max(range.e_mhz.1),
    );
    let all = mhz(m.cycles, cpu_ns);
    if all.is_some_and(|v| !within(v, whole)) {
        return Err("clock_range");
    }
    let p = mhz(m.p_cycles, p_ns);
    if p.is_some_and(|v| !within(v, range.p_mhz)) {
        return Err("p_clock_range");
    }
    let e_cycles = m.cycles - m.p_cycles;
    let efficiency = |whole_ns: u64| {
        let e = mhz(e_cycles, whole_ns.saturating_sub(p_ns));
        let refused = e.is_some_and(|v| !within(v, range.e_mhz))
            || (e_cycles > 0 && whole_ns <= p_ns && cpu_ns >= MIN_CLOCK_NS);
        (e.filter(|_| !refused), refused)
    };
    let (e, e_refused) = efficiency(cpu_ns);
    let (e_qos, e_qos_refused) = efficiency(qos_ns);
    let ipc =
        (cpu_ns >= MIN_CLOCK_NS && m.cycles > 0).then(|| m.instructions as f64 / m.cycles as f64);
    if ipc.is_some_and(|v| !(MIN_IPC..MAX_IPC).contains(&v)) {
        return Err("ipc");
    }
    if cpu_ns >= MIN_CLOCK_NS {
        let per_core = m.energy_nj as f64 / cpu_ns as f64;
        if !(CPU_W_PER_CORE.0..=CPU_W_PER_CORE.1).contains(&per_core) {
            return Err("energy");
        }
    }
    let window = window_ns as f64;
    let gpu_w = m.gpu_energy_nj as f64 / window;
    if gpu_w > MAX_GPU_W {
        return Err("gpu_energy");
    }
    let ane_w = m.ane_energy_nj as f64 / window;
    let ane_busy = ns(m.ane_ticks) as f64 / window;
    if ane_w > MAX_ANE_W || ane_busy > MAX_ANE_BUSY {
        return Err("ane");
    }
    Ok(Judged {
        cpu_s: cpu_ns as f64 / 1e9,
        qos_s: qos_ns as f64 / 1e9,
        p_s: p_ns as f64 / 1e9,
        mhz: all,
        p_mhz: p,
        e_mhz: e,
        e_refused,
        e_qos_mhz: e_qos,
        e_qos_refused,
        ipc,
        cpu_w: m.energy_nj as f64 / window,
        gpu_w,
        ane_w,
        ane_busy,
    })
}

#[cfg(test)]
#[path = "coalition_heat_tests.rs"]
mod tests;
