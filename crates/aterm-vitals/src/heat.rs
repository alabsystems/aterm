// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The heat chain (`docs/DESIGN-stats-deck-instruments-2026-11-06.md` §4, step N2): what the
//! machine's compute costs in power, and what the thermal limiter does to it.
//!
//! - The documented thermal pressure level ([`ThermalPressure`]: macOS's
//!   `com.apple.system.thermalpressurelevel`, 0 Nominal … 4 Sleeping), which the host reads only
//!   when its notification fires.
//! - The resource coalitions' heat counters ([`Counters`]): CPU, GPU and Neural Engine energy,
//!   instructions, cycles, the Super (performance) cores' cycles, instructions and time, and the
//!   CPU time by QoS. The host decodes each record in the coalition read's own copy loop and sums
//!   them there ([`HeatBook`]); the start counter's merge hands back the coalitions gone since the
//!   last read ([`crate::starts::coalition_moves`]), whose last counters come off the earlier total
//!   ([`change`]). No pass after the read runs (M-COAL: a separate pass costs 64.6 / 81.3 µs, the
//!   folded form 12.5 / 18.2 µs).
//! - Each change is judged against the chip and the machine ([`judge`]): a layout the self-check
//!   does not recognise reads unread, never as a shifted value.
//! - What the deck and `sys` read of it ([`HeatView`]): CPU, GPU and Neural Engine watts; the Super
//!   clock, ΔP cycles ÷ ΔP time; the Efficiency clock, (Δcycles − ΔP cycles) ÷ (ΔQoS time − ΔP
//!   time), never over `cpu_time` (which undercounts the busy cores, 0.926 p50 in N1); instructions
//!   per joule; the Super cores' share of the time and of the instructions.
//! - The usual Super clock per Super busy decile ([`UsualClocks`]), learned only at Nominal or
//!   Moderate from the same derived clock the cut reads, so a steady bias cancels in clock ÷ usual;
//!   and the cut ([`Cut`]), claimed only in a decile whose M-PCLK verdict passed ([`ClockGate`]).
//!   The gate is data: N1 passed no decile at Nominal or Moderate on any chip, so no reading claims
//!   a cut until a run there passes and its verdict here says so.
//!
//! Every value has an explicit absence ([`NoHeat`]): a system with no reader of it, or a reader
//! that gave nothing this reading. Nothing reads as a zero it did not measure.

use crate::explain::CoalLane;
use crate::model::{State, Thermal};

/// How many heat counters a coalition's record gives ([`Counters`]).
pub const COUNTERS: usize = 11;

/// A coalition's heat counters in [`Counters::words`] order: the form the host's copy loop keeps
/// per coalition and sums, so the model takes the read whole, without a copy.
pub type Words = [u64; COUNTERS];

/// Thread time under which a reading's clocks, IPC, CPU watts per core, work per joule and the
/// Super shares are not judged or given: a few ticks' rounding would decide them.
pub const MIN_CLOCK_NS: u64 = 10_000_000;

/// How far under a table's bottom a derived clock may read and still be the clock.
pub const BELOW_BOTTOM: f64 = 0.5;

/// How far over a table's top a derived clock may read and still be the clock.
pub const OVER_TOP: f64 = 1.02;

/// Instructions per cycle no core retires at or past.
pub const MAX_IPC: f64 = 16.0;

/// Instructions per cycle under which the words are not instructions and cycles.
pub const MIN_IPC: f64 = 0.01;

/// CPU watts per busy core (nanojoules per nanosecond of QoS time) a reading may give.
pub const CPU_W_PER_CORE: (f64, f64) = (0.01, 25.0);

/// GPU watts a reading may give.
pub const MAX_GPU_W: f64 = 200.0;

/// Neural Engine watts a reading may give.
pub const MAX_ANE_W: f64 = 50.0;

/// Neural Engine busy share (its time ÷ the window) a reading may give: two engines, and slack.
pub const MAX_ANE_BUSY: f64 = 2.1;

/// How far a reading's CPU time, its QoS time and its Super time may stray from the busy core time
/// the per-core ticks gave ([`Busy`]): a time word that moved to a neighbour reads near 0 or many
/// times the busy time.
pub const BUSY_RATIO: (f64, f64) = (0.5, 1.5);

/// How far the per-core ticks' window may stray from the heat change's and still give its busy
/// time: a reading whose cores were read over another window judges no busy time and no decile.
pub const BUSY_WINDOW_SLACK: f64 = 0.25;

/// Super busy deciles ([`decile`]).
pub const DECILES: usize = 10;

/// Seconds of readings a decile learns its usual clock over before the cut reads it: the
/// sustained 30 s every learned figure of the deck stands on.
pub const USUAL_LEARN_S: f64 = 30.0;

/// The weight, in seconds of readings, past which a decile's usual clock moves as an exponential
/// mean of that span: an hour of readings in the decile.
pub const USUAL_SPAN_S: f64 = 3_600.0;

/// How far under its usual the Super clock runs before the cut is claimed: 5 % (A4 §3).
pub const CUT_SHARE: f64 = 0.05;

/// One coalition's heat counters, as one record gave them (or a sum, or a change, of them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// CPU time, Mach ticks (`cpu_time`).
    pub cpu_ticks: u64,
    /// CPU time by effective QoS, summed, Mach ticks.
    pub qos_ticks: u64,
    /// CPU time on Super cores, Mach ticks (`cpu_ptime`).
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

impl Counters {
    /// The counters of `w`, in [`Self::words`] order.
    #[must_use]
    pub const fn from_words(w: Words) -> Self {
        Self {
            cpu_ticks: w[0],
            qos_ticks: w[1],
            p_ticks: w[2],
            energy_nj: w[3],
            instructions: w[4],
            cycles: w[5],
            p_instructions: w[6],
            p_cycles: w[7],
            ane_ticks: w[8],
            ane_energy_nj: w[9],
            gpu_energy_nj: w[10],
        }
    }

    /// The counters as words, in field order.
    #[must_use]
    pub const fn words(self) -> Words {
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

    /// Each counter's change from `then`; `None` where one ran backwards.
    #[must_use]
    pub fn since(self, then: Self) -> Option<Self> {
        sub(self.words(), then.words()).map(Self::from_words)
    }

    /// Whether the Super cycles and instructions lie within their wholes (one account each).
    #[must_use]
    pub const fn super_within_whole(self) -> bool {
        self.p_cycles <= self.cycles && self.p_instructions <= self.instructions
    }
}

/// `a − b` word by word; `None` where a word of `b` exceeds `a`'s.
fn sub(a: Words, b: Words) -> Option<Words> {
    let mut out = [0; COUNTERS];
    for (o, (x, y)) in out.iter_mut().zip(a.iter().zip(b)) {
        *o = x.checked_sub(y)?;
    }
    Some(out)
}

/// One coalition read's heat counters, as the host's copy loop left them (the read's records
/// decoded where the kernel copied them out, and summed there).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeatBook {
    /// The middle of the read, on the host's monotonic clock (ns): the change's window runs from
    /// one read's to the next's.
    pub at_ns: u64,
    /// The Mach timebase `(numer, denom)` the times tick at: `ticks × numer ÷ denom` is ns.
    pub timebase: (u32, u32),
    /// Each coalition that answered and decoded: its id and words, in the kernel's order (the
    /// kernel lists coalitions in no id order, so they are not sorted: a gone coalition's words are
    /// found by one pass, [`Self::sum_of`]).
    pub by_id: Vec<(u64, Words)>,
    /// [`Self::by_id`]'s words summed; the first refusal where a record of the read did not decode
    /// (its layout, an unwritten word, Super over whole) or the sum would pass `u64` (`overflow`),
    /// which leaves the reading's heat unread.
    pub total: Result<Words, &'static str>,
}

impl HeatBook {
    /// The words of the coalitions `ids` (ascending, as the merge hands them back) summed.
    ///
    /// # Errors
    /// `gone_unread` where one of them is not in this read; `overflow` where the sum would pass
    /// `u64`, as the copy loop's total refuses.
    pub fn sum_of(&self, ids: &[u64]) -> Result<Words, &'static str> {
        let mut sum = [0u64; COUNTERS];
        if ids.is_empty() {
            return Ok(sum);
        }
        let mut found = 0usize;
        for (id, w) in &self.by_id {
            if ids.binary_search(id).is_ok() {
                found += 1;
                for (s, v) in sum.iter_mut().zip(w) {
                    *s = s.checked_add(*v).ok_or("overflow")?;
                }
            }
        }
        if found == ids.len() {
            Ok(sum)
        } else {
            Err("gone_unread")
        }
    }
}

/// The heat counters' change over one window ([`change`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeatMove {
    /// The summed change: each coalition both reads decoded, each born since whole.
    pub counters: Counters,
    /// From the earlier read's middle to the later's, ns.
    pub window_ns: u64,
    /// The Mach timebase both reads tick at.
    pub timebase: (u32, u32),
}

/// The change from `then` to `now`, given the coalitions gone since `then` (`gone`, ascending ids,
/// as [`crate::starts::coalition_moves`] hands them back): `now`'s total less what `then`'s total
/// kept, the gone coalitions' last counters taken off it. Equal to the change by id (each matched
/// coalition's change plus each born one whole) wherever the start counter's merge counts, which
/// refuses a coalition with no base.
///
/// # Errors
/// Either read's own refusal; `timebase` where the reads tick differently; `window` where `now`
/// is not after `then`; `gone_unread` where a gone coalition's words are not in `then`;
/// `overflow` where their sum would pass `u64`; `backwards` where a counter ran backwards.
pub fn change(then: &HeatBook, now: &HeatBook, gone: &[u64]) -> Result<HeatMove, &'static str> {
    let was = then.total?;
    let is = now.total?;
    if then.timebase != now.timebase {
        return Err("timebase");
    }
    let window_ns = now
        .at_ns
        .checked_sub(then.at_ns)
        .filter(|w| *w > 0)
        .ok_or("window")?;
    let left = then.sum_of(gone)?;
    let kept = sub(was, left).ok_or("backwards")?;
    let moved = sub(is, kept).ok_or("backwards")?;
    Ok(HeatMove {
        counters: Counters::from_words(moved),
        window_ns,
        timebase: now.timebase,
    })
}

/// `ticks` of a Mach clock at `(numer, denom)` in ns.
#[must_use]
pub fn ticks_to_ns(ticks: u64, (numer, denom): (u32, u32)) -> u64 {
    if denom == 0 {
        return ticks;
    }
    let ns = u128::from(ticks) * u128::from(numer) / u128::from(denom);
    u64::try_from(ns).unwrap_or(u64::MAX)
}

/// The chip's clock range: each core kind's DVFS table bottom and top, MHz, as the host read the
/// power manager's tables once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockRange {
    /// Super cores' bottom and top.
    pub p_mhz: (f64, f64),
    /// Efficiency cores' bottom and top.
    pub e_mhz: (f64, f64),
}

impl ClockRange {
    /// Whether `mhz` lies within `(bottom, top)` as a derived clock may ([`BELOW_BOTTOM`],
    /// [`OVER_TOP`]).
    #[must_use]
    pub fn holds(mhz: f64, (bottom, top): (f64, f64)) -> bool {
        mhz >= bottom * BELOW_BOTTOM && mhz <= top * OVER_TOP
    }

    /// Both kinds' range together.
    #[must_use]
    pub fn whole(&self) -> (f64, f64) {
        (
            self.p_mhz.0.min(self.e_mhz.0),
            self.p_mhz.1.max(self.e_mhz.1),
        )
    }
}

/// The busy core time the machine's per-core ticks gave over a reading's window, ns of core: every
/// core's and the Super cores'.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Busy {
    /// Every core's busy time.
    pub all_ns: u64,
    /// The Super cores' busy time; `None` where the machine has one kind of core, whose Super
    /// time is not checked.
    pub p_ns: Option<u64>,
}

/// The Efficiency clock, (cycles − Super cycles) ÷ (`whole_ns` − Super time) in MHz, over a whole
/// time account `whole_ns`: `Ok(None)` under [`MIN_CLOCK_NS`] of Efficiency time; `Err` where it
/// lies past the Efficiency table's range (`e_range`), or where cycles ran with no time left for
/// them. The engine gives it over the QoS time ([`judge`]); N1's comparison gives it over
/// `cpu_time` too, by this one rule.
///
/// # Errors
/// `past_table` as above.
pub fn e_clock(
    e_cycles: u64,
    whole_ns: u64,
    p_ns: u64,
    e_range: Option<(f64, f64)>,
) -> Result<Option<f64>, &'static str> {
    let e_ns = whole_ns.saturating_sub(p_ns);
    if e_cycles > 0 && whole_ns <= p_ns && whole_ns >= MIN_CLOCK_NS {
        return Err("past_table");
    }
    if e_ns < MIN_CLOCK_NS {
        return Ok(None);
    }
    let mhz = e_cycles as f64 * 1e3 / e_ns as f64;
    match e_range {
        Some(r) if !ClockRange::holds(mhz, r) => Err("past_table"),
        _ => Ok(Some(mhz)),
    }
}

/// A judged change ([`judge`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Judged {
    /// CPU time (`cpu_time`), seconds of core.
    pub cpu_s: f64,
    /// CPU time by effective QoS, seconds of core: the account that matches the busy cores.
    pub qos_s: f64,
    /// CPU time on Super cores, seconds of core.
    pub p_s: f64,
    /// Every core's cycles ÷ the QoS time, MHz; `None` under [`MIN_CLOCK_NS`] or with no clock
    /// range.
    pub mhz: Option<f64>,
    /// Super cycles ÷ Super time, MHz; `None` under [`MIN_CLOCK_NS`] or with no clock range.
    pub p_mhz: Option<f64>,
    /// The Efficiency clock over the QoS time ([`e_clock`]), MHz; `None` under [`MIN_CLOCK_NS`],
    /// with no clock range, or past the Efficiency table's range ([`Self::e_refused`]).
    pub e_mhz: Option<f64>,
    /// Whether [`Self::e_mhz`] read past its table's range and was refused, alone: the reading
    /// stands.
    pub e_refused: bool,
    /// Instructions per cycle; `None` under [`MIN_CLOCK_NS`].
    pub ipc: Option<f64>,
    /// Instructions per joule of CPU energy; `None` under [`MIN_CLOCK_NS`] or with no energy.
    pub ipj: Option<f64>,
    /// The Super cores' share of the QoS time; `None` under [`MIN_CLOCK_NS`].
    pub p_time_share: Option<f64>,
    /// The Super cores' share of the instructions; `None` under [`MIN_CLOCK_NS`].
    pub p_instruction_share: Option<f64>,
    /// CPU watts over the window.
    pub cpu_w: f64,
    /// GPU watts over the window.
    pub gpu_w: f64,
    /// Neural Engine watts over the window.
    pub ane_w: f64,
    /// Neural Engine time ÷ the window.
    pub ane_busy: f64,
}

/// Judges a change `m` over a `window_ns` window at the Mach `timebase` against the chip's clock
/// `range` (where read) and, where given, the `busy` core time of the same window.
///
/// The whole clock, the CPU watts per busy core and the time gates read the QoS time, the account
/// that matches the busy cores (0.995 p50 of them in N1); `cpu_time` undercounts them (0.926 p50,
/// 0.737 at worst), so a clock over it reads up to a third high, past the table's top on a cool
/// machine whose Super cores run near it.
///
/// # Errors
/// The first invariant that failed: `window`, `super_over_whole`, `busy_time`, `busy_qos_time`,
/// `busy_p_time`, `clock_range`, `p_clock_range`, `ipc`, `energy`, `gpu_energy`, `ane`.
pub fn judge(
    m: &Counters,
    window_ns: u64,
    timebase: (u32, u32),
    range: Option<&ClockRange>,
    busy: Option<Busy>,
) -> Result<Judged, &'static str> {
    if window_ns == 0 {
        return Err("window");
    }
    if !m.super_within_whole() {
        return Err("super_over_whole");
    }
    let ns = |ticks: u64| ticks_to_ns(ticks, timebase);
    let (cpu_ns, qos_ns, p_ns) = (ns(m.cpu_ticks), ns(m.qos_ticks), ns(m.p_ticks));
    let strays = |time: u64, busy: u64| {
        (busy >= MIN_CLOCK_NS || time >= MIN_CLOCK_NS)
            && !(BUSY_RATIO.0..=BUSY_RATIO.1).contains(&(time as f64 / busy.max(1) as f64))
    };
    if let Some(busy) = busy {
        if strays(cpu_ns, busy.all_ns) {
            return Err("busy_time");
        }
        if strays(qos_ns, busy.all_ns) {
            return Err("busy_qos_time");
        }
        if busy.p_ns.is_some_and(|busy_p| strays(p_ns, busy_p)) {
            return Err("busy_p_time");
        }
    }
    let mhz =
        |cycles: u64, ns: u64| (ns >= MIN_CLOCK_NS).then_some(cycles as f64 * 1e3 / ns as f64);
    let all = range.and_then(|_| mhz(m.cycles, qos_ns));
    if let (Some(v), Some(r)) = (all, range)
        && !ClockRange::holds(v, r.whole())
    {
        return Err("clock_range");
    }
    let p = range.and_then(|_| mhz(m.p_cycles, p_ns));
    if let (Some(v), Some(r)) = (p, range)
        && !ClockRange::holds(v, r.p_mhz)
    {
        return Err("p_clock_range");
    }
    let (e, e_refused) = match range {
        Some(r) => match e_clock(m.cycles - m.p_cycles, qos_ns, p_ns, Some(r.e_mhz)) {
            Ok(e) => (e, false),
            Err(_) => (None, true),
        },
        None => (None, false),
    };
    let timed = qos_ns >= MIN_CLOCK_NS;
    let ipc = (timed && m.cycles > 0).then_some(m.instructions as f64 / m.cycles as f64);
    if ipc.is_some_and(|v| !(MIN_IPC..MAX_IPC).contains(&v)) {
        return Err("ipc");
    }
    if timed {
        let per_core = m.energy_nj as f64 / qos_ns as f64;
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
        ipc,
        ipj: (timed && m.energy_nj > 0).then_some(m.instructions as f64 * 1e9 / m.energy_nj as f64),
        p_time_share: timed.then_some(p_ns as f64 / qos_ns as f64),
        p_instruction_share: (timed && m.instructions > 0)
            .then_some(m.p_instructions as f64 / m.instructions as f64),
        cpu_w: m.energy_nj as f64 / window,
        gpu_w,
        ane_w,
        ane_busy,
    })
}

/// The busy core time over `window_ns` of the per-core busy fractions `fracs`, read over
/// `cores_window_ns`, every core's and the `p_cores`' (indices into `fracs`; `None` with one kind
/// of core): `None` where the two windows stray past [`BUSY_WINDOW_SLACK`] of each other.
#[must_use]
pub fn busy_of(
    fracs: &[f64],
    p_cores: Option<&[usize]>,
    cores_window_ns: u64,
    window_ns: u64,
) -> Option<Busy> {
    if fracs.is_empty() || window_ns == 0 {
        return None;
    }
    let stray = (cores_window_ns as f64 - window_ns as f64).abs() / window_ns as f64;
    if stray > BUSY_WINDOW_SLACK {
        return None;
    }
    let w = window_ns as f64;
    let all: f64 = fracs.iter().sum();
    let p = p_cores.map(|p| p.iter().filter_map(|&i| fracs.get(i)).sum::<f64>());
    Some(Busy {
        all_ns: (all * w) as u64,
        p_ns: p.map(|p| (p * w) as u64),
    })
}

/// The Super busy decile of `share`, the Super cores' busy share of their capacity:
/// ⌊10 × share⌋, a full share in decile 9; `None` for a share that is not a number in `0..=1`.
#[must_use]
pub fn decile(share: f64) -> Option<usize> {
    if !(0.0..=1.0).contains(&share) {
        return None;
    }
    Some(((share * DECILES as f64) as usize).min(DECILES - 1))
}

/// The documented thermal pressure level (`OSThermalNotification.h`'s
/// `kOSThermalPressureLevel*`), which moves where `NSProcessInfo.thermalState` does not (A4 §4:
/// Moderate → Heavy at 10:10:27 while thermalState stayed Fair).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ThermalPressure {
    /// 0: no pressure.
    Nominal,
    /// 1: the system is warm; its limiter may start to act.
    Moderate,
    /// 2: the limiter is acting.
    Heavy,
    /// 3: the system is shedding work.
    Trapping,
    /// 4: the system is about to sleep for heat.
    Sleeping,
}

impl ThermalPressure {
    /// The level's posted state (`0..=4`); `None` for any other, never a guessed level.
    #[must_use]
    pub const fn of_state(state: u64) -> Option<Self> {
        Some(match state {
            0 => Self::Nominal,
            1 => Self::Moderate,
            2 => Self::Heavy,
            3 => Self::Trapping,
            4 => Self::Sleeping,
            _ => return None,
        })
    }

    /// The level's number, `0..=4`: also how many of the ladder's four steps it lights.
    #[must_use]
    pub const fn level(self) -> u8 {
        self as u8
    }

    /// The level's word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::Moderate => "moderate",
            Self::Heavy => "heavy",
            Self::Trapping => "trapping",
            Self::Sleeping => "sleeping",
        }
    }

    /// Whether a reading at this level teaches the usual clock: Nominal or Moderate (A4 §4).
    #[must_use]
    pub const fn learns(self) -> bool {
        matches!(self, Self::Nominal | Self::Moderate)
    }

    /// Whether a cut is judged at this level: Moderate or more (A4 §3).
    #[must_use]
    pub const fn judges_cut(self) -> bool {
        !matches!(self, Self::Nominal)
    }

    /// The well's state at this level: warn at Heavy, critical at Trapping or Sleeping; none
    /// under Heavy, where the ladder alone says the level.
    #[must_use]
    pub const fn state(self) -> Option<State> {
        match self {
            Self::Nominal | Self::Moderate => None,
            Self::Heavy => Some(State::Warn),
            Self::Trapping | Self::Sleeping => Some(State::Critical),
        }
    }
}

/// A measurement's verdict on one derived clock (§16's M-PCLK per Super busy decile, M-ECLK for the
/// Efficiency clock), held as data: only [`Verdict::Pass`] lets a reading claim what it gates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// Not measured on this chip: claims nothing.
    #[default]
    Unmeasured,
    /// Measured and failed: claims nothing.
    Fail,
    /// Within its line by too thin a margin to stand on (one more tail reading fails it).
    Marginal,
    /// Passed at pressure Heavy only, where the usual clock is not learned: whether the bias
    /// cancels between Nominal or Moderate and Heavy is unmeasured, so it claims nothing.
    HeavyOnly,
    /// Passed where it is used.
    Pass,
}

impl Verdict {
    /// The verdict's word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Unmeasured => "unmeasured",
            Self::Fail => "fail",
            Self::Marginal => "marginal",
            Self::HeavyOnly => "heavy_only",
            Self::Pass => "pass",
        }
    }
}

/// The derived clocks' gates for one chip: M-PCLK's verdict per Super busy decile (the cut) and
/// M-ECLK's (the Efficiency clock drawn filled).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockGate {
    /// The P clock's verdict per Super busy decile.
    pub p: [Verdict; DECILES],
    /// The Efficiency clock's verdict.
    pub e: Verdict,
}

impl ClockGate {
    /// What N1 measured on Apple M5 under xnu-13432 (`n1/coal.md`), against the complex CPU Stats
    /// channels the recordings read: deciles 0–5 unmeasured (0–12 readings each), 6 and 7 failed
    /// (spread 3.01 % and 3.05 %), 8 marginal (1.42 %, 5 of 101 readings past 2 %), 9 passed at
    /// Heavy only (0.18 %, n 863); the Efficiency clock failed (17.4 % within 5 % at low E load).
    pub const APPLE_M5: Self = Self {
        p: [
            Verdict::Unmeasured,
            Verdict::Unmeasured,
            Verdict::Unmeasured,
            Verdict::Unmeasured,
            Verdict::Unmeasured,
            Verdict::Unmeasured,
            Verdict::Fail,
            Verdict::Fail,
            Verdict::Marginal,
            Verdict::HeavyOnly,
        ],
        e: Verdict::Fail,
    };

    /// The gate measured for the chip `brand` names (`machdep.cpu.brand_string`): N1's for an
    /// `Apple M5`, nothing measured for any other.
    #[must_use]
    pub fn of_chip(brand: Option<&str>) -> Self {
        match brand {
            Some("Apple M5") => Self::APPLE_M5,
            _ => Self::default(),
        }
    }
}

/// One decile's usual Super clock ([`UsualClocks`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usual {
    /// Seconds of readings it learned from, ever.
    pub read_s: f64,
    /// The usual clock, MHz: the readings' window-weighted mean, an exponential one of
    /// [`USUAL_SPAN_S`] once that much was read.
    pub mhz: f64,
}

/// The usual Super clock per Super busy decile, learned only at Nominal or Moderate, from the same
/// derived clock the cut reads (A4 §4). The model keeps it in memory; N3 writes it to the strip's
/// file beside the peaks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UsualClocks {
    /// Each decile's, 0 to 9.
    pub deciles: [Usual; DECILES],
}

impl UsualClocks {
    /// Notes a reading of `mhz` over `window_s` seconds in `decile`; nothing for a clock or window
    /// that is not a positive number, or a decile past 9.
    pub fn note(&mut self, decile: usize, mhz: f64, window_s: f64) {
        let Some(u) = self.deciles.get_mut(decile) else {
            return;
        };
        if !(mhz.is_finite() && mhz > 0.0 && window_s.is_finite() && window_s > 0.0) {
            return;
        }
        let kept = u.read_s.min(USUAL_SPAN_S);
        u.mhz = (u.mhz * kept + mhz * window_s) / (kept + window_s);
        u.read_s += window_s;
    }

    /// `decile`'s usual clock, once [`USUAL_LEARN_S`] of readings taught it.
    #[must_use]
    pub fn usual(&self, decile: usize) -> Option<f64> {
        self.deciles
            .get(decile)
            .filter(|u| u.read_s >= USUAL_LEARN_S)
            .map(|u| u.mhz)
    }
}

/// Whether the Super clock is cut, and why not where it is not claimed (A4 §3, §4).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Cut {
    /// The pressure is under Moderate, or unread: no cut is judged.
    #[default]
    NotJudged,
    /// No Super clock or no Super busy decile this reading.
    Unread,
    /// The decile's M-PCLK verdict claims nothing ([`ClockGate`]).
    Gated(Verdict),
    /// The decile has not learned its usual clock.
    Unlearned,
    /// Within [`CUT_SHARE`] of the usual: the usual ÷ the clock.
    Within(f64),
    /// More than [`CUT_SHARE`] under the usual: the usual ÷ the clock.
    Cut(f64),
}

impl Cut {
    /// The cut at pressure `pressure` of Super clock `p_mhz` in `decile`, whose verdict is
    /// `verdict` and usual clock `usual` (in that order of refusal).
    #[must_use]
    pub fn judge(
        pressure: Option<ThermalPressure>,
        p_mhz: Option<f64>,
        decile: Option<usize>,
        verdict: Verdict,
        usual: Option<f64>,
    ) -> Self {
        if !pressure.is_some_and(ThermalPressure::judges_cut) {
            return Self::NotJudged;
        }
        let (Some(clock), Some(_)) = (p_mhz.filter(|c| *c > 0.0), decile) else {
            return Self::Unread;
        };
        if verdict != Verdict::Pass {
            return Self::Gated(verdict);
        }
        let Some(usual) = usual else {
            return Self::Unlearned;
        };
        let ratio = usual / clock;
        if clock < usual * (1.0 - CUT_SHARE) {
            Self::Cut(ratio)
        } else {
            Self::Within(ratio)
        }
    }

    /// The Heat row's cause score (A4 §3): 1 + log2(usual ÷ clock) ÷ 3 under a cut, the capacity
    /// it took on the CPU arm's scale; 0 otherwise, whatever the level (the ladder and the well
    /// say the level).
    #[must_use]
    pub fn score(self) -> f64 {
        match self {
            Self::Cut(ratio) => 1.0 + ratio.log2() / 3.0,
            _ => 0.0,
        }
    }
}

/// Why a heat value is not given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoHeat {
    /// This system has no reader of it: its unit is absent and keeps no width (A3's rule).
    NotSupported(&'static str),
    /// Its reader runs and gave nothing this reading: its unit draws hollow.
    Unread(&'static str),
}

/// The heat chain's derived values over one window: what the coalitions' counters gave.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chain {
    /// The window, ns.
    pub window_ns: u64,
    /// The summed change of the counters.
    pub counters: Counters,
    /// The chip's clock range the clocks were judged against, or why there is none (the clocks
    /// are then not given).
    pub range: Result<ClockRange, NoHeat>,
    /// What the self-check judged and derived of it.
    pub judged: Judged,
    /// The Super busy decile from the per-core ticks; `None` where the cores were not read over
    /// the window, or the machine has one kind of core.
    pub decile: Option<usize>,
    /// The decile's usual Super clock, MHz, before this reading taught it; `None` until learned.
    pub usual_mhz: Option<f64>,
    /// The decile's M-PCLK verdict ([`ClockGate::p`]); [`Verdict::Unmeasured`] with no decile.
    pub p_verdict: Verdict,
    /// The cut.
    pub cut: Cut,
    /// M-ECLK's verdict: the Efficiency clock draws filled only at [`Verdict::Pass`].
    pub e_verdict: Verdict,
}

impl Chain {
    /// The Efficiency clock as the deck draws it filled: only where its bound held and M-ECLK
    /// passed; otherwise the tile is hollow ([`Judged::e_mhz`] still says the value).
    #[must_use]
    pub fn e_drawn(&self) -> Option<f64> {
        (self.e_verdict == Verdict::Pass)
            .then_some(self.judged.e_mhz)
            .flatten()
    }
}

/// CPU, GPU and Neural Engine watts over a reading's window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Watts {
    /// The CPU's.
    pub cpu: Result<f64, NoHeat>,
    /// The GPU's.
    pub gpu: Result<f64, NoHeat>,
    /// The Neural Engine's.
    pub ane: Result<f64, NoHeat>,
}

impl Watts {
    /// Every part `why`.
    #[must_use]
    pub const fn none(why: NoHeat) -> Self {
        Self {
            cpu: Err(why),
            gpu: Err(why),
            ane: Err(why),
        }
    }

    /// Compute watts, the Heat row's figure: CPU + GPU + Neural Engine. A part this system has no
    /// reader of adds nothing; with the CPU's absent, or any part unread, the figure is too.
    ///
    /// # Errors
    /// The CPU's absence, else the first part's that is unread.
    pub fn compute(&self) -> Result<f64, NoHeat> {
        let mut sum = self.cpu?;
        for part in [self.gpu, self.ane] {
            match part {
                Ok(w) => sum += w,
                Err(NoHeat::NotSupported(_)) => {}
                Err(why) => return Err(why),
            }
        }
        Ok(sum)
    }
}

/// Watts a system reads outside the coalitions (A4 §4's Linux sources: RAPL's CPU package energy
/// and NVML's GPU board power), as the host read them. They stand where no coalition chain reads;
/// where one does, its own watts stand.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostWatts {
    /// The CPU's.
    pub cpu: Result<f64, NoHeat>,
    /// The GPU's.
    pub gpu: Result<f64, NoHeat>,
}

impl Default for HostWatts {
    /// A system with neither reader (Windows: A4 §4 names none).
    fn default() -> Self {
        let why = NoHeat::NotSupported("no_power_reader");
        Self {
            cpu: Err(why),
            gpu: Err(why),
        }
    }
}

/// The heat chain at one reading: the Snapshot's `heat`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeatView {
    /// The documented thermal pressure level.
    pub pressure: Result<ThermalPressure, NoHeat>,
    /// CPU, GPU and Neural Engine watts.
    pub watts: Watts,
    /// The coalitions' chain: clocks, work per joule, the Super shares, the cut.
    pub chain: Result<Chain, NoHeat>,
}

impl Default for HeatView {
    fn default() -> Self {
        let why = NoHeat::Unread("not_read");
        Self {
            pressure: Err(why),
            watts: Watts::none(why),
            chain: Err(why),
        }
    }
}

impl HeatView {
    /// How many of the pressure ladder's four steps are lit: the level (Heavy lights two); none
    /// while the pressure is unread.
    #[must_use]
    pub fn steps(&self) -> u8 {
        self.pressure.map_or(0, ThermalPressure::level)
    }

    /// The Heat well's state (A4 §4): the worse of the pressure's (Heavy or more) and `thermal`'s,
    /// the OS's documented thermal state at serious or critical ([`Thermal::state`]).
    #[must_use]
    pub fn well(&self, thermal: Option<Thermal>) -> Option<State> {
        let pressure = self.pressure.ok().and_then(ThermalPressure::state);
        pressure.max(thermal.and_then(Thermal::state))
    }

    /// The Heat row's cause score ([`Cut::score`]); 0 with no chain.
    #[must_use]
    pub fn score(&self) -> f64 {
        self.chain.map_or(0.0, |c| c.cut.score())
    }
}

/// What one reading gives the heat chain ([`Engine::absorb`]).
#[derive(Clone, Copy, Debug)]
pub struct Input<'a> {
    /// The coalition lane's state: unsupported off macOS, off once a read misread its layout.
    pub lane: CoalLane,
    /// The pressure as the host read it: `None` on a system with no documented level.
    pub pressure: Option<Result<ThermalPressure, &'static str>>,
    /// The counters' change this reading, or why there is none; `None` where the reading's start
    /// counter carries no heat (Linux's forks).
    pub moved: Option<Result<HeatMove, &'static str>>,
    /// The chip's clock range as the host read it once: `None` on a system with no table of it.
    pub range: Option<Result<ClockRange, &'static str>>,
    /// The chip's clock gates.
    pub gate: ClockGate,
    /// Each logical core's busy fraction this reading and the window they were read over, ns.
    pub cores: Option<(&'a [f64], u64)>,
    /// The Super cores' indices, where the machine has more than one kind of core.
    pub p_cores: Option<&'a [usize]>,
    /// The system's own watts readers, for a system with no coalitions.
    pub host: HostWatts,
}

/// The heat chain's state across readings: the usual clocks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Engine {
    /// The usual Super clock per decile.
    pub usual: UsualClocks,
}

impl Engine {
    /// The heat chain at a reading of `i`: judges the change, gives its decile, judges the cut
    /// against the decile's usual clock as learned before this reading, then teaches the usual
    /// clock this reading's at Nominal or Moderate.
    pub fn absorb(&mut self, i: &Input<'_>) -> HeatView {
        let pressure = match i.pressure {
            None => Err(NoHeat::NotSupported("no_pressure_level")),
            Some(Err(why)) => Err(NoHeat::Unread(why)),
            Some(Ok(p)) => Ok(p),
        };
        let chain = match (i.lane, i.moved) {
            (CoalLane::Unsupported, _) => Err(NoHeat::NotSupported("no_coalitions")),
            (CoalLane::Off, _) => Err(NoHeat::Unread("lane_off")),
            (CoalLane::On, None) => Err(NoHeat::Unread("no_heat_read")),
            (CoalLane::On, Some(Err(why))) => Err(NoHeat::Unread(why)),
            (CoalLane::On, Some(Ok(m))) => self.chain(&m, i, pressure.ok()),
        };
        let watts = match chain {
            Ok(c) => Watts {
                cpu: Ok(c.judged.cpu_w),
                gpu: Ok(c.judged.gpu_w),
                ane: Ok(c.judged.ane_w),
            },
            // No coalitions here: the system's own readers, and no Neural Engine to read.
            Err(NoHeat::NotSupported(_)) => Watts {
                cpu: i.host.cpu,
                gpu: i.host.gpu,
                ane: Err(NoHeat::NotSupported("no_neural_engine")),
            },
            Err(why) => Watts::none(why),
        };
        HeatView {
            pressure,
            watts,
            chain,
        }
    }

    fn chain(
        &mut self,
        m: &HeatMove,
        i: &Input<'_>,
        pressure: Option<ThermalPressure>,
    ) -> Result<Chain, NoHeat> {
        let p_cores = i.p_cores.filter(|p| !p.is_empty());
        let busy = i
            .cores
            .and_then(|(fracs, cores_window)| busy_of(fracs, p_cores, cores_window, m.window_ns));
        let range = match i.range {
            None => Err(NoHeat::NotSupported("no_clock_table")),
            Some(Err(why)) => Err(NoHeat::Unread(why)),
            Some(Ok(r)) => Ok(r),
        };
        let judged = judge(
            &m.counters,
            m.window_ns,
            m.timebase,
            range.as_ref().ok(),
            busy,
        )
        .map_err(NoHeat::Unread)?;
        let decile = match (busy.and_then(|b| b.p_ns), p_cores) {
            (Some(busy_p), Some(p)) => {
                decile(busy_p as f64 / (p.len() as f64 * m.window_ns as f64))
            }
            _ => None,
        };
        let usual_mhz = decile.and_then(|d| self.usual.usual(d));
        let p_verdict = decile.map_or(Verdict::Unmeasured, |d| i.gate.p[d]);
        let cut = Cut::judge(pressure, judged.p_mhz, decile, p_verdict, usual_mhz);
        if let (Some(d), Some(p), Some(level)) = (decile, judged.p_mhz, pressure)
            && level.learns()
        {
            self.usual.note(d, p, m.window_ns as f64 / 1e9);
        }
        Ok(Chain {
            window_ns: m.window_ns,
            counters: m.counters,
            range,
            judged,
            decile,
            usual_mhz,
            p_verdict,
            cut,
            e_verdict: i.gate.e,
        })
    }
}

#[cfg(test)]
#[path = "heat_tests.rs"]
mod tests;
