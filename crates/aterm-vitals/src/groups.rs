// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, memory-only groups of reported work. The model supplies proved member contributions
//! before display capping; this book adds no process reader, ownership proof or counter baseline.
//! Reported process and coalition CPU are independent counter accounts, never a physical partition.

use std::sync::Arc;

use crate::delegated::ProcessKey;
use crate::model::Unread;
use crate::ring::{
    Cell, CompactAgg, Lands, MultiScaleSpec, ScaleBand, Taken, TimeGap, lands, slot_gap_kind,
    slot_weight,
};

/// Maximum app groups, including retained ended groups.
pub const APP_GROUPS: usize = 128;
/// Maximum outside groups, including retained ended groups.
pub const OUTSIDE_GROUPS: usize = 32;
/// Maximum retained groups.
pub const GROUPS: usize = APP_GROUPS + OUTSIDE_GROUPS;
/// Ended groups are eligible for removal after one hour on the engine's monotonic clock.
pub const ENDED_KEEP_MS: u64 = 3_600_000;
/// Maximum bytes of a retained UTF-8 executable label, including a truncation ellipsis.
pub const LABEL_BYTES: usize = 80;
/// Retained storage budget for one book or its immutable view, including backing allocations.
pub const BOOK_BYTES: usize = 256 * 1024;

/// A stable root identity, in the engine's current boot. A PID alone never makes a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GroupKey {
    /// The shell child whose proved descendants belong to this pane.
    Tab {
        /// Original pane/custody key, not a displayed tab number.
        pane: u64,
        /// Kernel generation of the established shell-child root.
        root: ProcessKey,
    },
    /// Work rooted in this app, outside a pane.
    Aterm(ProcessKey),
    /// Independently proved shared app work.
    Shared(ProcessKey),
    /// One resource coalition, whose whole counter account may have mixed ownership.
    Coalition(u64),
}

impl GroupKey {
    /// Whether this key belongs to the app quota.
    #[must_use]
    pub const fn in_aterm(self) -> bool {
        !matches!(self, Self::Coalition(_))
    }
}

/// Current classification. A tab key is the model's pane/custody key, not a displayed tab number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    /// A proved pane origin.
    Tab(u64),
    /// This app outside a pane.
    Aterm,
    /// Independently proved shared app work.
    Shared,
    /// A canonically identified outside user account.
    Others,
    /// A canonically identified service account.
    Services,
    /// Mixed or unproved outside ownership.
    #[default]
    Unknown,
}

/// A reported point and its missing contributions. Partial is not a physical bound or partition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<T> {
    /// Every contribution needed for this reported value read.
    Read(T),
    /// The supplied reported contributions, with some contributions unread.
    Partial {
        /// Sum of the readable reported contributions.
        value: T,
        /// Contributions that did not read.
        unread: u32,
    },
    /// No reported value; a known zero is represented by `Read(0)` instead.
    Unread {
        /// Canonical typed absence.
        why: Unread,
        /// Contributions that did not read.
        unread: u32,
    },
}

impl<T> Value<T> {
    /// No observation, with the number of missing contributions explicitly carried.
    #[must_use]
    pub const fn not_sampled(unread: u32) -> Self {
        Self::Unread {
            why: Unread::NotSampled,
            unread,
        }
    }

    /// No supported source for this quantity.
    #[must_use]
    pub const fn unsupported() -> Self {
        Self::Unread {
            why: Unread::Unsupported,
            unread: 0,
        }
    }

    /// Number of unread contributions, including a numeric partial point's missing members.
    #[must_use]
    pub const fn unread(&self) -> u32 {
        match self {
            Self::Read(_) => 0,
            Self::Partial { unread, .. } | Self::Unread { unread, .. } => *unread,
        }
    }

    /// The absence reason where no point is reported.
    #[must_use]
    pub const fn why(&self) -> Option<Unread> {
        match self {
            Self::Unread { why, .. } => Some(*why),
            _ => None,
        }
    }
}

impl<T: Copy> Value<T> {
    /// The reported value, including a qualified partial point.
    #[must_use]
    pub const fn value(self) -> Option<T> {
        match self {
            Self::Read(value) | Self::Partial { value, .. } => Some(value),
            Self::Unread { .. } => None,
        }
    }

    /// A fully read point, for histories whose cells cannot retain member-level partial coverage.
    #[must_use]
    pub const fn complete_value(self) -> Option<T> {
        match self {
            Self::Read(value) => Some(value),
            _ => None,
        }
    }
}

impl<T> Default for Value<T> {
    fn default() -> Self {
        Self::not_sampled(1)
    }
}

/// Source read separation only. It proves no atomic accounting or physical CPU execution window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadWindow {
    /// Original reported source endpoint and read separation, ns on `Sample::at_ns`'s clock.
    Reported {
        /// Original source read endpoint on the engine's source clock.
        end_ns: u64,
        /// Original reported read separation; not a physical execution window.
        window_ns: u64,
    },
    /// Contributions came from different source read separations or endpoints.
    Mixed,
    /// No source separation read.
    #[default]
    Unread,
}

/// A reported rate and its own source read window; energy never borrows CPU's window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rate {
    /// The reported rate, with unread contributions retained.
    pub value: Value<f64>,
    /// Original source read separation.
    pub window: ReadWindow,
}

impl Rate {
    /// No supported source for this rate.
    #[must_use]
    pub const fn unsupported() -> Self {
        Self {
            value: Value::unsupported(),
            window: ReadWindow::Unread,
        }
    }
}

/// What the CPU point reports, never a physical CPU partition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CpuBasis {
    /// Sum of each member's published own process counter rate; no descendant CPU is added.
    #[default]
    ProcessOwnCounters,
    /// One coalition's independent published counter rate.
    CoalitionCounters,
}

/// Separate energy counter channels, in milliwatts. GPU energy never means GPU busy time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Energy {
    /// The process's reported `ri_energy_nj` thread ledger; not asserted to be CPU-only.
    pub thread_mw: Rate,
    /// CPU energy from the accepted coalition heat transition.
    pub cpu_mw: Rate,
    /// GPU energy from that same accepted transition.
    pub gpu_mw: Rate,
    /// Neural Engine energy from that same accepted transition.
    pub ane_mw: Rate,
}

impl Default for Energy {
    fn default() -> Self {
        Self {
            thread_mw: Rate::unsupported(),
            cpu_mw: Rate::unsupported(),
            gpu_mw: Rate::unsupported(),
            ane_mw: Rate::unsupported(),
        }
    }
}

/// One existing model reading. `taken` is the row engine's unchanged canonical `Taken`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// Original model publication sequence, not a later GUI publication's sequence.
    pub seq: u64,
    /// Canonical wall-clock slot and cadence, used by the shared ring rules.
    pub taken: Taken,
    /// Existing engine monotonic clock, ms, for bounded ended retention.
    pub mono_ms: u64,
    /// Existing engine source clock, ns, not substituted for any rate's endpoint.
    pub at_ns: u64,
    /// This original reading actually contains a process capture, possibly incomplete.
    pub processes_observed: bool,
    /// This original reading actually contains a coalition capture, possibly incomplete.
    pub coalitions_observed: bool,
    /// A whole process observation may prove an app group absent.
    pub processes_complete: bool,
    /// A whole coalition observation may prove an outside group absent.
    pub coalitions_complete: bool,
}

/// One proved member contribution, or one whole outside coalition account. Repeated app keys
/// are accumulated inside the bounded book; callers do not pre-cap the outside observations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Observation<'a> {
    /// Stable model-proved origin or coalition key.
    pub key: GroupKey,
    /// App scope must agree with the key; outside classification needs canonical evidence.
    pub scope: Scope,
    /// Root-key-bound executable label. Empty preserves an earlier label of this same root.
    pub name: &'a str,
    /// Observed members in this contribution; normally one for an app member.
    pub count: u32,
    /// Reported own CPU, in cores-worth.
    pub cpu: Rate,
    /// Which independent account CPU reports.
    pub cpu_basis: CpuBasis,
    /// Physical footprint charges, bytes, with missing contributions retained.
    pub footprint: Value<u64>,
    /// Storage reads, bytes/s.
    pub read_bps: Rate,
    /// Storage writes, bytes/s; unsupported for an undecoded coalition source.
    pub write_bps: Rate,
    /// Original separately windowed energy channels.
    pub energy: Energy,
    /// Process starts/s where the accepted original transition reports them.
    pub starts_ps: Rate,
    /// Whether every member needed for this membership observation was known.
    pub membership_complete: bool,
}

impl<'a> Observation<'a> {
    /// An observation whose costs are unread until the authoritative producer supplies them.
    #[must_use]
    pub fn new(key: GroupKey, scope: Scope, name: &'a str, count: u32) -> Self {
        Self {
            key,
            scope,
            name,
            count,
            cpu: Rate::default(),
            cpu_basis: if key.in_aterm() {
                CpuBasis::ProcessOwnCounters
            } else {
                CpuBasis::CoalitionCounters
            },
            footprint: Value::not_sampled(count.max(1)),
            read_bps: Rate::default(),
            write_bps: Rate::unsupported(),
            energy: Energy::default(),
            starts_ps: Rate::unsupported(),
            membership_complete: false,
        }
    }
}

/// Contributions the hard storage bounds or malformed inputs omitted in this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Omitted {
    /// App member contributions omitted; not falsely described as distinct groups.
    pub app_members: u64,
    /// Whole outside coalition accounts not retained in the top 32.
    pub outside_groups: u64,
    /// Contributions whose key and scope/basis did not agree.
    pub invalid: u64,
}

/// A resource history the group strip shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    /// Reported CPU cores-worth.
    Cpu,
    /// Footprint charges, bytes.
    Memory,
}

const BANDS: [ScaleBand; 4] = [
    ScaleBand {
        slots: 1,
        period_ms: 1_000,
        weight: 1,
        live_now: true,
    },
    ScaleBand {
        slots: 5,
        period_ms: 10_000,
        weight: 1,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 60_000,
        weight: 1,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 600_000,
        weight: 1,
        live_now: false,
    },
];
/// Sixteen boxes, newest first, from the same slot geometry used by the strip planner.
pub const HISTORY_SPEC: MultiScaleSpec = MultiScaleSpec {
    bands: &BANDS,
    ticks: &[],
};

/// Temporal qualification of a projected group box. These are reported source-slot statistics,
/// never measured physical CPU execution or continuous resource ownership.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryQuality {
    /// No supported source interval can be assigned to this box.
    #[default]
    Unread,
    /// The aggregate's represented source slots are inside the box, or its contiguous constant
    /// source permits a conservative clip without estimating its values.
    Reported,
    /// A contiguous source bin overlaps this box, but its changing values cannot be separated:
    /// mean/max describe that bin and estimate this box, rather than exact window statistics.
    Binned,
}

impl HistoryQuality {
    /// Stable machine-readable qualification.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::Reported => "reported",
            Self::Binned => "binned_estimate",
        }
    }
}

/// One projected group box. `cell.tau_ms` is represented source-slot weight, not elapsed read
/// coverage or an execution window. `quality` explicitly qualifies changing partial-bin values.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HistoryCell {
    /// Weighted source statistics; a binned estimate's maximum may lie outside this box.
    pub cell: Cell,
    /// The same shared gap rule the main strips use.
    pub gap: Option<crate::ring::GapKind>,
    /// The fixed sixteen-box progressive geometry.
    pub slot: crate::ring::ProgSlot,
    /// Whether values are supported aggregates or changing partial-bin estimates.
    pub quality: HistoryQuality,
}

/// On-demand projection: no second set of history arrays is retained by the book or Snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct History {
    cpu: [HistoryCell; 16],
    memory: [HistoryCell; 16],
}

impl History {
    fn unread() -> Self {
        let slots: Vec<_> = HISTORY_SPEC.iter_slots().collect();
        let cells = std::array::from_fn(|i| HistoryCell {
            slot: slots[i],
            ..HistoryCell::default()
        });
        Self {
            cpu: cells,
            memory: cells,
        }
    }

    /// The sixteen boxes, newest first.
    #[must_use]
    pub fn cells(&self, metric: Metric) -> &[HistoryCell; 16] {
        match metric {
            Metric::Cpu => &self.cpu,
            Metric::Memory => &self.memory,
        }
    }
}

const CPU: usize = 0;
const READ: usize = 1;
const WRITE: usize = 2;
const STARTS: usize = 3;
const THREAD: usize = 4;
const ENERGY_CPU: usize = 5;
const ENERGY_GPU: usize = 6;
const ENERGY_ANE: usize = 7;
const RATES: usize = 8;
const BUCKETS: usize = 7;
const PERIODS: [i64; 3] = [10_000, 60_000, 600_000];

// Two conservative 15-bit support offsets leave flags for contiguous and constant input. A
// coarse bin rounds its bounds outward, then clips only the inner interval it can prove. This
// retains temporal authority without expanding 42 bins into a recent-reading reservoir.
const OFFSET_MASK: u32 = (1 << 15) - 1;
const CONTIGUOUS: u32 = 1 << 30;
const CONSTANT: u32 = 1 << 31;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
struct SupportedAgg {
    mean: f64,
    max: f64,
    tau_ms: u32,
    support: u32,
}

const _: () = assert!(std::mem::size_of::<SupportedAgg>() <= 24);

impl SupportedAgg {
    fn quantum(period: i64) -> i64 {
        (period + i64::from(OFFSET_MASK) - 1) / i64::from(OFFSET_MASK)
    }

    fn bounds(self, period: i64) -> (i64, i64) {
        let q = Self::quantum(period);
        (
            i64::from(self.support & OFFSET_MASK) * q,
            i64::from((self.support >> 15) & OFFSET_MASK) * q,
        )
    }

    fn projection(self) -> CompactAgg {
        CompactAgg::from_projection(self.mean, self.max, self.tau_ms)
    }

    fn cell(self) -> Cell {
        self.projection().cell()
    }

    fn note(
        &mut self,
        value: f64,
        weight: u32,
        from: i64,
        to: i64,
        bucket_lo: i64,
        period: i64,
        covered_before: i64,
    ) {
        if weight == 0 || to <= from {
            return;
        }
        let first = self.tau_ms == 0;
        let contiguous = first || (self.support & CONTIGUOUS != 0 && from <= covered_before);
        let constant =
            first || (self.support & CONSTANT != 0 && self.mean.to_bits() == value.to_bits());
        let (old_lo, old_hi) = self.bounds(period);
        let lo = from.saturating_sub(bucket_lo).clamp(0, period);
        let hi = to.saturating_sub(bucket_lo).clamp(0, period);
        let q = Self::quantum(period);
        let lo = (if first { lo } else { lo.min(old_lo) }) / q;
        let hi = (if first { hi } else { hi.max(old_hi) } + q - 1) / q;
        let flags =
            (if contiguous { CONTIGUOUS } else { 0 }) | (if constant { CONSTANT } else { 0 });
        self.support = u32::try_from(lo).unwrap_or(0).min(OFFSET_MASK)
            | (u32::try_from(hi).unwrap_or(OFFSET_MASK).min(OFFSET_MASK) << 15)
            | flags;
        let mut agg = self.projection();
        agg.add(value, weight, false);
        let cell = agg.cell();
        self.mean = cell.mean.unwrap_or(0.0);
        self.max = cell.max.unwrap_or(0.0);
        self.tau_ms = cell.tau_ms as u32;
    }

    fn part(self, bucket_lo: i64, period: i64, lo: i64, hi: i64) -> Option<(Cell, HistoryQuality)> {
        if self.tau_ms == 0 {
            return None;
        }
        let (first, last) = self.bounds(period);
        let outer_lo = i128::from(bucket_lo) + i128::from(first);
        let outer_hi = i128::from(bucket_lo) + i128::from(last);
        let (lo, hi) = (i128::from(lo), i128::from(hi));
        if hi <= outer_lo || lo >= outer_hi {
            return None;
        }
        if lo <= outer_lo && hi >= outer_hi {
            return Some((self.cell(), HistoryQuality::Reported));
        }
        if self.support & CONTIGUOUS == 0 {
            return None;
        }
        let q = Self::quantum(period);
        // With quantized outer bounds, one quantum inward is a guaranteed represented span.
        let inward = if q == 1 { 0 } else { i128::from(q) };
        let from = lo.max(outer_lo + inward);
        let to = hi.min(outer_hi - inward);
        if to <= from {
            return None;
        }
        let weight = (to - from).min(i128::from(self.tau_ms)) as f64;
        let quality = if self.support & CONSTANT != 0 {
            HistoryQuality::Reported
        } else {
            HistoryQuality::Binned
        };
        Some((
            Cell {
                mean: Some(self.mean),
                max: Some(self.max),
                tau_ms: weight,
                over_ms: 0.0,
            },
            quality,
        ))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
struct StoredValue {
    bits: u64,
    unread: u32,
    kind: u8,
    why: u8,
}

fn why_code(why: Unread) -> u8 {
    match why {
        Unread::Unsupported => 0,
        Unread::NotSampled => 1,
        Unread::Refused => 2,
        Unread::UnreadFormat => 3,
    }
}
fn why_value(why: u8) -> Unread {
    match why {
        0 => Unread::Unsupported,
        2 => Unread::Refused,
        3 => Unread::UnreadFormat,
        _ => Unread::NotSampled,
    }
}

impl StoredValue {
    fn missing(why: Unread, unread: u32) -> Self {
        Self {
            bits: 0,
            unread,
            kind: 0,
            why: why_code(why),
        }
    }
    fn float(value: Value<f64>) -> Self {
        let unread = value.unread();
        match value.value() {
            Some(v) if v.is_finite() && v >= 0.0 => Self {
                bits: v.to_bits(),
                unread,
                kind: 1,
                why: 1,
            },
            Some(_) => Self::missing(Unread::UnreadFormat, unread.max(1)),
            None => Self::missing(value.why().unwrap_or(Unread::NotSampled), unread),
        }
    }
    fn integer(value: Value<u64>) -> Self {
        match value.value() {
            Some(v) => Self {
                bits: v,
                unread: value.unread(),
                kind: 1,
                why: 1,
            },
            None => Self::missing(value.why().unwrap_or(Unread::NotSampled), value.unread()),
        }
    }
    fn value<T>(self, value: T) -> Value<T> {
        if self.kind == 0 {
            Value::Unread {
                why: why_value(self.why),
                unread: self.unread,
            }
        } else if self.unread > 0 {
            Value::Partial {
                value,
                unread: self.unread,
            }
        } else {
            Value::Read(value)
        }
    }
    fn as_float(self) -> Value<f64> {
        self.value(f64::from_bits(self.bits))
    }
    fn as_integer(self) -> Value<u64> {
        self.value(self.bits)
    }
    fn add(&mut self, other: Self, integer: bool) {
        let unread = self.unread.saturating_add(other.unread);
        match (self.kind != 0, other.kind != 0) {
            (true, true) => {
                let sum = if integer {
                    self.bits.checked_add(other.bits)
                } else {
                    let v = f64::from_bits(self.bits) + f64::from_bits(other.bits);
                    v.is_finite().then_some(v.to_bits())
                };
                *self = sum.map_or_else(
                    || Self::missing(Unread::UnreadFormat, unread.max(1)),
                    |bits| Self {
                        bits,
                        unread,
                        kind: 1,
                        why: 1,
                    },
                );
            }
            (false, true) => {
                *self = other;
                self.unread = unread;
            }
            (true, false) => self.unread = unread,
            (false, false) => {
                self.unread = unread;
                self.why = self.why.max(other.why);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Facts {
    values: [StoredValue; RATES],
    ends: [u64; RATES],
    spans: [u64; RATES],
    windows: [u8; RATES],
}

impl Default for Facts {
    fn default() -> Self {
        Self {
            values: [StoredValue::missing(Unread::NotSampled, 1); RATES],
            ends: [0; RATES],
            spans: [0; RATES],
            windows: [0; RATES],
        }
    }
}

impl Facts {
    fn window(&self, index: usize) -> ReadWindow {
        match self.windows[index] {
            1 => ReadWindow::Reported {
                end_ns: self.ends[index],
                window_ns: self.spans[index],
            },
            2 => ReadWindow::Mixed,
            _ => ReadWindow::Unread,
        }
    }
    fn rate(&self, index: usize) -> Rate {
        Rate {
            value: self.values[index].as_float(),
            window: self.window(index),
        }
    }
    fn put(&mut self, index: usize, rate: Rate, add: bool) {
        let incoming = StoredValue::float(rate.value);
        let had_value = self.values[index].kind != 0;
        let window = if add && had_value && incoming.kind != 0 && self.window(index) != rate.window
        {
            ReadWindow::Mixed
        } else if add && had_value {
            self.window(index)
        } else {
            rate.window
        };
        if add {
            self.values[index].add(incoming, false);
        } else {
            self.values[index] = incoming;
        }
        let (tag, end, span) = match window {
            ReadWindow::Reported { end_ns, window_ns } if window_ns > 0 && end_ns >= window_ns => {
                (1, end_ns, window_ns)
            }
            ReadWindow::Mixed => (2, 0, 0),
            _ => (0, 0, 0),
        };
        (self.windows[index], self.ends[index], self.spans[index]) = (tag, end, span);
    }
    fn observe(&mut self, o: Observation<'_>, add: bool) {
        for (i, r) in [
            o.cpu,
            o.read_bps,
            o.write_bps,
            o.starts_ps,
            o.energy.thread_mw,
            o.energy.cpu_mw,
            o.energy.gpu_mw,
            o.energy.ane_mw,
        ]
        .into_iter()
        .enumerate()
        {
            self.put(i, r, add);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Name {
    bytes: [u8; LABEL_BYTES],
    len: u8,
    truncated: bool,
    held: bool,
}
impl Default for Name {
    fn default() -> Self {
        Self {
            bytes: [0; LABEL_BYTES],
            len: 0,
            truncated: false,
            held: false,
        }
    }
}
impl Name {
    fn set(&mut self, name: &str) {
        if name.is_empty() {
            return;
        }
        let truncated = name.len() > LABEL_BYTES;
        let mut end = name.len().min(if truncated {
            LABEL_BYTES - 3
        } else {
            LABEL_BYTES
        });
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        self.bytes[..end].copy_from_slice(&name.as_bytes()[..end]);
        if truncated {
            self.bytes[end..end + 3].copy_from_slice("…".as_bytes());
            end += 3;
        }
        self.len = u8::try_from(end).unwrap_or(0);
        self.truncated = truncated;
        self.held = false;
    }
    fn get(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Axis {
    open: Option<i64>,
    newest_ms: Option<i64>,
    cursor: u8,
}
#[derive(Clone, Copy, Debug, Default)]
struct Advance {
    clear: u8,
    shift_ms: i128,
}
impl Axis {
    fn advance(&mut self, t: Taken, period: i64) -> Advance {
        let mut out = Advance::default();
        match lands(self.open, self.newest_ms, t.at_ms, period) {
            Lands::First(k) => {
                self.open = Some(k);
                self.cursor = 0;
                out.clear = 127;
            }
            Lands::Newest => {}
            Lands::Ahead(by) => {
                self.open = Some(t.at_ms.div_euclid(period));
                if by >= BUCKETS as i64 {
                    self.cursor = 0;
                    out.clear = 127;
                } else {
                    for _ in 0..by {
                        self.cursor = (self.cursor + 1) % BUCKETS as u8;
                        out.clear |= 1 << self.cursor;
                    }
                }
            }
            Lands::Back(by) => {
                self.open = Some(self.open.unwrap_or(0).saturating_sub(by).saturating_add(1));
                self.cursor = (self.cursor + 1) % BUCKETS as u8;
                out.clear = 1 << self.cursor;
                out.shift_ms = -i128::from(by) * i128::from(period);
                self.newest_ms = self
                    .newest_ms
                    .and_then(|v| i64::try_from(i128::from(v) + out.shift_ms).ok());
            }
        }
        if self.newest_ms.is_none_or(|n| t.at_ms >= n) {
            self.newest_ms = Some(t.at_ms);
        }
        out
    }
    fn index(self, k: i64) -> Option<usize> {
        let back = usize::try_from(i128::from(self.open?) - i128::from(k)).ok()?;
        (back < BUCKETS).then(|| (usize::from(self.cursor) + BUCKETS - back) % BUCKETS)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct KeptHistory {
    cells: [[[SupportedAgg; 2]; BUCKETS]; 3],
    covered_to: [[i64; 2]; 3],
}
impl Default for KeptHistory {
    fn default() -> Self {
        Self {
            cells: [[[SupportedAgg::default(); 2]; BUCKETS]; 3],
            covered_to: [[i64::MIN; 2]; 3],
        }
    }
}
impl KeptHistory {
    fn advance(&mut self, changes: [Advance; 3]) {
        for (tier, change) in changes.into_iter().enumerate() {
            for i in 0..BUCKETS {
                if change.clear & (1 << i) != 0 {
                    self.cells[tier][i] = [SupportedAgg::default(); 2];
                }
            }
            if change.shift_ms != 0 {
                for covered in &mut self.covered_to[tier] {
                    *covered =
                        i64::try_from(i128::from(*covered) + change.shift_ms).unwrap_or(i64::MIN);
                }
            }
        }
    }
    fn note(&mut self, t: Taken, axes: [Axis; 3], values: [Option<f64>; 2]) {
        for (tier, axis) in axes.into_iter().enumerate() {
            let Some(k) = axis.open else {
                continue;
            };
            for (metric, value) in values.into_iter().enumerate() {
                if let Some(value) = value {
                    let before = self.covered_to[tier][metric];
                    let period = PERIODS[tier];
                    let (weight, end) = slot_weight(t, k, period, before);
                    let (slot_lo, _) = t.slot();
                    let bucket_lo = k.saturating_mul(period);
                    let from = slot_lo.max(before).max(bucket_lo);
                    self.covered_to[tier][metric] = end;
                    self.cells[tier][usize::from(axis.cursor)][metric]
                        .note(value, weight, from, end, bucket_lo, period, before);
                }
            }
        }
    }
    fn project(
        &self,
        metric: usize,
        axes: [Axis; 3],
        frame: Frame,
        live: Option<f64>,
        gaps: &[TimeGap],
    ) -> [HistoryCell; 16] {
        let slots: Vec<_> = HISTORY_SPEC.iter_slots().collect();
        std::array::from_fn(|i| {
            let slot = slots[i];
            let (lo, hi) = (
                frame.taken.at_ms.saturating_sub(slot.age_hi_ms),
                frame.taken.at_ms.saturating_sub(slot.age_lo_ms),
            );
            let (mut cell, mut quality) = if slot.is_live_now {
                live.map_or_else(
                    || (Cell::default(), HistoryQuality::Unread),
                    |v| {
                        (
                            Cell {
                                mean: Some(v),
                                max: Some(v),
                                tau_ms: 0.0,
                                over_ms: 0.0,
                            },
                            HistoryQuality::Reported,
                        )
                    },
                )
            } else {
                let tier = usize::from(slot.band.saturating_sub(1)).min(2);
                let period = PERIODS[tier];
                let axis = axes[tier];
                let now_k = frame.taken.at_ms.div_euclid(period);
                let k_lo = lo.div_euclid(period);
                let k_hi = hi.saturating_sub(1).max(lo).div_euclid(period).min(now_k);
                let parts = (k_lo..=k_hi)
                    .filter_map(|k| {
                        let bin = self.cells[tier][axis.index(k)?][metric];
                        let b_lo = i64::try_from(i128::from(k) * i128::from(period)).ok()?;
                        bin.part(b_lo, period, lo, hi)
                    })
                    .collect::<Vec<_>>();
                let quality = if parts.is_empty() {
                    HistoryQuality::Unread
                } else if parts
                    .iter()
                    .any(|(_, quality)| *quality == HistoryQuality::Binned)
                {
                    HistoryQuality::Binned
                } else {
                    HistoryQuality::Reported
                };
                let mut aggregate = CompactAgg::default();
                for (cell, _) in parts {
                    if let (Some(mean), Some(max)) = (cell.mean, cell.max) {
                        aggregate.merge(CompactAgg::from_projection(mean, max, cell.tau_ms as u32));
                    }
                }
                (aggregate.cell(), quality)
            };
            let (gap, clear) = if slot.is_live_now && live.is_some() {
                (None, false)
            } else {
                slot_gap_kind(gaps, lo, hi, cell.mean.is_some())
            };
            if clear {
                cell = Cell::default();
                quality = HistoryQuality::Unread;
            }
            HistoryCell {
                cell,
                gap,
                slot,
                quality,
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    key: GroupKey,
    scope: Scope,
    name: Name,
    facts: Facts,
    footprint: StoredValue,
    history: Option<Box<KeptHistory>>,
    count: u32,
    ended_since_ms: u64,
    ended: bool,
    seen: bool,
    membership_complete: bool,
    duplicate: bool,
}

const _: () = assert!(std::mem::size_of::<Entry>() + std::mem::size_of::<KeptHistory>() <= 1536);

impl Entry {
    fn new(o: Observation<'_>) -> Self {
        let mut entry = Self {
            key: o.key,
            scope: o.scope,
            name: Name::default(),
            facts: Facts::default(),
            footprint: StoredValue::missing(Unread::NotSampled, 1),
            history: None,
            count: 0,
            ended_since_ms: 0,
            ended: false,
            seen: false,
            membership_complete: false,
            duplicate: false,
        };
        entry.observe(o);
        entry
    }
    fn reset(&mut self, changes: [Advance; 3]) {
        if let Some(history) = &mut self.history {
            history.advance(changes);
        }
        self.name.held = self.name.len > 0;
        self.facts = Facts::default();
        self.footprint = StoredValue::missing(Unread::NotSampled, 1);
        self.count = 0;
        self.seen = false;
        self.membership_complete = false;
        self.duplicate = false;
    }
    fn observe(&mut self, o: Observation<'_>) {
        let add = self.seen;
        if add && !self.key.in_aterm() {
            self.facts = Facts::default();
            for v in &mut self.facts.values {
                *v = StoredValue::missing(Unread::UnreadFormat, 1);
            }
            self.footprint = StoredValue::missing(Unread::UnreadFormat, 1);
            self.duplicate = true;
            self.membership_complete = false;
            return;
        }
        if self.duplicate {
            return;
        }
        self.name.set(o.name);
        self.scope = o.scope;
        self.facts.observe(o, add);
        if add {
            self.footprint.add(StoredValue::integer(o.footprint), true);
        } else {
            self.footprint = StoredValue::integer(o.footprint);
        }
        self.count = self.count.saturating_add(o.count);
        self.membership_complete = if add {
            self.membership_complete && o.membership_complete
        } else {
            o.membership_complete
        };
        self.seen = true;
        self.ended = false;
    }
    fn cpu(&self) -> Option<f64> {
        self.facts.values[CPU].as_float().value()
    }
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + 2 * std::mem::size_of::<usize>()
            + self
                .history
                .as_ref()
                .map_or(0, |_| std::mem::size_of::<KeptHistory>())
    }
}

/// One immutable retained group. The packed facts and history are shared with its Snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct Row(Arc<Entry>);
impl Row {
    /// Stable root/coalition identity.
    #[must_use]
    pub fn key(&self) -> GroupKey {
        self.0.key
    }
    /// Proved app scope or current outside classification.
    #[must_use]
    pub fn scope(&self) -> Scope {
        self.0.scope
    }
    /// Root-bound, bounded executable label; empty means no label was read.
    #[must_use]
    pub fn name(&self) -> &str {
        self.0.name.get()
    }
    /// The same root's label is retained from an earlier observation.
    #[must_use]
    pub fn name_held(&self) -> bool {
        self.0.name.held
    }
    /// The root label exceeded the storage byte bound.
    #[must_use]
    pub fn name_truncated(&self) -> bool {
        self.0.name.truncated
    }
    /// Observed member count, qualified by `membership_complete`.
    #[must_use]
    pub fn count(&self) -> u32 {
        self.0.count
    }
    /// Current reported CPU point.
    #[must_use]
    pub fn cpu(&self) -> Rate {
        self.0.facts.rate(CPU)
    }
    /// Which independent published CPU account this key represents.
    #[must_use]
    pub fn cpu_basis(&self) -> CpuBasis {
        if self.0.key.in_aterm() {
            CpuBasis::ProcessOwnCounters
        } else {
            CpuBasis::CoalitionCounters
        }
    }
    /// Current footprint charges.
    #[must_use]
    pub fn footprint(&self) -> Value<u64> {
        self.0.footprint.as_integer()
    }
    /// Current storage read rate.
    #[must_use]
    pub fn read_bps(&self) -> Rate {
        self.0.facts.rate(READ)
    }
    /// Current storage write rate.
    #[must_use]
    pub fn write_bps(&self) -> Rate {
        self.0.facts.rate(WRITE)
    }
    /// Current starts rate.
    #[must_use]
    pub fn starts_ps(&self) -> Rate {
        self.0.facts.rate(STARTS)
    }
    /// Separate process/thread and coalition CPU/GPU/ANE energy channels.
    #[must_use]
    pub fn energy(&self) -> Energy {
        Energy {
            thread_mw: self.0.facts.rate(THREAD),
            cpu_mw: self.0.facts.rate(ENERGY_CPU),
            gpu_mw: self.0.facts.rate(ENERGY_GPU),
            ane_mw: self.0.facts.rate(ENERGY_ANE),
        }
    }
    /// No generation-authoritative group GPU busy source currently exists.
    #[must_use]
    pub fn gpu(&self) -> Value<f64> {
        Value::unsupported()
    }
    /// Whether every member required for the current membership observation read.
    #[must_use]
    pub fn membership_complete(&self) -> bool {
        self.0.membership_complete
    }
    /// A whole source observation proved the group ended.
    #[must_use]
    pub fn ended(&self) -> bool {
        self.0.ended
    }
}

/// Immutable bounded Snapshot input. No raw process census or retained PID-member table enters it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    rows: Arc<[Row]>,
    axes: [Axis; 3],
    frame: Frame,
    sampled: bool,
    omitted: Omitted,
}
impl View {
    /// All retained rows in stable, hysteretic order, before caller filtering and `top=`.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }
    /// What this frame omitted under hard bounds or invalid inputs.
    #[must_use]
    pub fn omitted(&self) -> Omitted {
        self.omitted
    }
    /// The original model frame, not a replacement rate endpoint.
    #[must_use]
    pub fn frame(&self) -> Option<Frame> {
        self.sampled.then_some(self.frame)
    }
    /// Project exactly the boxes the group picture reads, borrowing this Snapshot's gaps.
    #[must_use]
    pub fn history(&self, row: &Row, gaps: &[TimeGap]) -> History {
        if !self
            .rows
            .iter()
            .any(|current| Arc::ptr_eq(&current.0, &row.0))
        {
            return History::unread();
        }
        let Some(history) = row.0.history.as_deref() else {
            return History::unread();
        };
        let cpu = row
            .membership_complete()
            .then(|| row.cpu().value.complete_value())
            .flatten();
        let memory = row
            .membership_complete()
            .then(|| row.footprint().complete_value())
            .flatten()
            .map(|v| v as f64);
        History {
            cpu: history.project(0, self.axes, self.frame, cpu, gaps),
            memory: history.project(1, self.axes, self.frame, memory, gaps),
        }
    }
    /// Charged backing storage, including shared Arc payloads and arrays. Allocator-specific slab
    /// overhead and a caller's stack are not measured by this layout/capacity accounting.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + 2 * std::mem::size_of::<usize>()
            + self.rows.len() * std::mem::size_of::<Row>()
            + self
                .rows
                .iter()
                .map(|row| row.0.retained_bytes())
                .sum::<usize>()
    }
}

/// Model-owned bounded group history. Its only inputs are the existing model's proved facts.
#[derive(Clone, Debug)]
pub struct GroupBook {
    rows: Vec<Row>,
    axes: [Axis; 3],
    frame: Frame,
    sampled: bool,
    omitted: Omitted,
}
impl Default for GroupBook {
    fn default() -> Self {
        Self {
            rows: Vec::with_capacity(GROUPS),
            axes: [Axis::default(); 3],
            frame: Frame::default(),
            sampled: false,
            omitted: Omitted::default(),
        }
    }
}

fn valid(o: Observation<'_>) -> bool {
    match (o.key, o.scope, o.cpu_basis) {
        (GroupKey::Tab { pane, root }, Scope::Tab(scope), CpuBasis::ProcessOwnCounters) => {
            pane == scope && root.pid > 1
        }
        (GroupKey::Aterm(root), Scope::Aterm, CpuBasis::ProcessOwnCounters)
        | (GroupKey::Shared(root), Scope::Shared, CpuBasis::ProcessOwnCounters) => root.pid > 1,
        (
            GroupKey::Coalition(_),
            Scope::Others | Scope::Services | Scope::Unknown,
            CpuBasis::CoalitionCounters,
        ) => true,
        _ => false,
    }
}

fn score_cmp(a: &Entry, b: &Entry) -> std::cmp::Ordering {
    match (a.cpu(), b.cpu()) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
    }
    .then_with(|| b.key.cmp(&a.key))
}

fn leads(challenger: &Entry, incumbent: &Entry) -> bool {
    let Some((c, i)) = challenger.cpu().zip(incumbent.cpu()) else {
        return false;
    };
    c > i && (c * 10.0 >= i * 11.0 || c >= i + 0.1)
}

impl GroupBook {
    /// An empty book; no observation or zero value is invented by construction.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume an uncapped stream. App entries may repeat per member; an outside account occurs
    /// once. Quotas include ended rows. Only whole source observations can establish an ending.
    pub fn absorb<'a>(
        &mut self,
        frame: Frame,
        observations: impl IntoIterator<Item = Observation<'a>>,
    ) {
        self.frame = frame;
        self.sampled = true;
        self.omitted = Omitted::default();
        self.rows.retain(|r| {
            !r.0.ended || frame.mono_ms.saturating_sub(r.0.ended_since_ms) < ENDED_KEEP_MS
        });
        let changes = std::array::from_fn(|i| self.axes[i].advance(frame.taken, PERIODS[i]));
        for row in &mut self.rows {
            Arc::make_mut(&mut row.0).reset(changes);
        }
        // Bounded facts only: histories remain with incumbents until the whole stream proves
        // which entries survive. No input ordering can prematurely evict a still-live history.
        let mut pending: Vec<Row> = Vec::with_capacity(GROUPS);
        for o in observations {
            if !valid(o)
                || !frame.processes_observed
                || (!o.key.in_aterm() && !frame.coalitions_observed)
            {
                self.omitted.invalid = self.omitted.invalid.saturating_add(1);
                continue;
            }
            if let Some(index) = self.rows.iter().position(|r| r.key() == o.key) {
                if !o.key.in_aterm() && self.rows[index].0.seen {
                    self.omitted.invalid = self.omitted.invalid.saturating_add(1);
                }
                Arc::make_mut(&mut self.rows[index].0).observe(o);
                continue;
            }
            if let Some(index) = pending.iter().position(|r| r.key() == o.key) {
                if !o.key.in_aterm() {
                    self.omitted.invalid = self.omitted.invalid.saturating_add(1);
                }
                Arc::make_mut(&mut pending[index].0).observe(o);
                continue;
            }
            let app = o.key.in_aterm();
            let quota = if app { APP_GROUPS } else { OUTSIDE_GROUPS };
            let candidate = Entry::new(o);
            let used = pending.iter().filter(|r| r.key().in_aterm() == app).count();
            let replace = (used >= quota && !app)
                .then(|| {
                    pending
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| !r.key().in_aterm())
                        .min_by(|(_, a), (_, b)| score_cmp(&a.0, &b.0))
                        .map(|(i, _)| i)
                })
                .flatten()
                .filter(|&i| score_cmp(&candidate, &pending[i].0).is_gt());
            if used >= quota && replace.is_none() {
                if app {
                    self.omitted.app_members =
                        self.omitted.app_members.saturating_add(u64::from(o.count));
                } else {
                    self.omitted.outside_groups = self.omitted.outside_groups.saturating_add(1);
                }
                continue;
            }
            if let Some(index) = replace {
                self.omitted.outside_groups = self.omitted.outside_groups.saturating_add(1);
                pending.remove(index);
            }
            pending.push(Row(Arc::new(candidate)));
        }
        for row in &mut self.rows {
            let entry = Arc::make_mut(&mut row.0);
            let whole = if entry.key.in_aterm() {
                frame.processes_observed && frame.processes_complete
            } else {
                frame.coalitions_observed && frame.coalitions_complete
            };
            if (!entry.seen || (entry.count == 0 && entry.membership_complete))
                && whole
                && !entry.ended
            {
                entry.ended = true;
                entry.ended_since_ms = frame.mono_ms;
            }
        }
        for mut candidate in pending {
            let app = candidate.key().in_aterm();
            let quota = if app { APP_GROUPS } else { OUTSIDE_GROUPS };
            let used = self
                .rows
                .iter()
                .filter(|r| r.key().in_aterm() == app)
                .count();
            let replace = if used < quota {
                None
            } else {
                self.rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.key().in_aterm() == app && r.ended())
                    .min_by_key(|(_, r)| (r.0.ended_since_ms, r.key()))
                    .map(|(i, _)| i)
                    .or_else(|| {
                        (!app)
                            .then(|| {
                                self.rows
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, r)| !r.key().in_aterm())
                                    .min_by(|(_, a), (_, b)| score_cmp(&a.0, &b.0))
                                    .map(|(i, _)| i)
                            })
                            .flatten()
                            .filter(|&i| score_cmp(&candidate.0, &self.rows[i].0).is_gt())
                    })
            };
            if used >= quota && replace.is_none() {
                if app {
                    self.omitted.app_members = self
                        .omitted
                        .app_members
                        .saturating_add(u64::from(candidate.count()));
                } else {
                    self.omitted.outside_groups = self.omitted.outside_groups.saturating_add(1);
                }
                continue;
            }
            if let Some(index) = replace {
                if !app && self.rows[index].0.seen {
                    self.omitted.outside_groups = self.omitted.outside_groups.saturating_add(1);
                }
                self.rows.remove(index);
            }
            let entry = Arc::make_mut(&mut candidate.0);
            entry.history = Some(Box::new(KeptHistory::default()));
            let whole = if app {
                frame.processes_observed && frame.processes_complete
            } else {
                frame.coalitions_observed && frame.coalitions_complete
            };
            if entry.count == 0 && entry.membership_complete && whole {
                entry.ended = true;
                entry.ended_since_ms = frame.mono_ms;
            }
            self.rows.push(candidate);
        }
        for row in &mut self.rows {
            let entry = Arc::make_mut(&mut row.0);
            let values = if entry.membership_complete {
                [
                    entry.facts.values[CPU].as_float().complete_value(),
                    entry
                        .footprint
                        .as_integer()
                        .complete_value()
                        .map(|v| v as f64),
                ]
            } else {
                [None, None]
            };
            if let Some(history) = &mut entry.history {
                history.note(frame.taken, self.axes, values);
            }
        }
        // The margin is not a total ordering. Select the next leader against that rank's own
        // incumbent; stable ties and incomparable/unread points keep their previous position.
        for rank in 0..self.rows.len() {
            if self.rows[rank].ended() {
                if let Some(index) = (rank + 1..self.rows.len()).find(|&i| !self.rows[i].ended()) {
                    let row = self.rows.remove(index);
                    self.rows.insert(rank, row);
                }
            }
            let mut winner = rank;
            for i in rank + 1..self.rows.len() {
                if !self.rows[i].ended()
                    && self.rows[i]
                        .0
                        .cpu()
                        .zip(self.rows[winner].0.cpu())
                        .is_some_and(|(a, b)| a > b)
                {
                    winner = i;
                }
            }
            if winner != rank && leads(&self.rows[winner].0, &self.rows[rank].0) {
                let row = self.rows.remove(winner);
                self.rows.insert(rank, row);
            }
        }
    }

    /// Immutable bounded Snapshot view. Entries are shared, never a copied census or member map.
    #[must_use]
    pub fn view(&self) -> View {
        View {
            rows: self.rows.clone().into(),
            axes: self.axes,
            frame: self.frame,
            sampled: self.sampled,
            omitted: self.omitted,
        }
    }

    /// Charged capacities and Arc/Box payload sizes for one retained version. A caller keeping
    /// older immutable views owns those older versions; they are not hidden inside this book.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.rows.capacity() * std::mem::size_of::<Row>()
            + self
                .rows
                .iter()
                .map(|row| row.0.retained_bytes())
                .sum::<usize>()
    }

    /// Maximum bounded facts-only staging charge during `absorb`, separate from retained history.
    /// Borrowed input storage and older views retained by consumers are not owned by this scratch.
    #[must_use]
    pub fn frame_scratch_bytes() -> usize {
        std::mem::size_of::<Vec<Row>>()
            + std::mem::size_of::<Entry>()
            + GROUPS
                * (std::mem::size_of::<Row>()
                    + std::mem::size_of::<Entry>()
                    + 2 * std::mem::size_of::<usize>())
    }
}

#[cfg(test)]
#[path = "groups_tests.rs"]
mod tests;
