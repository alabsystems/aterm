// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ten-second wall-clock bucketed hour ring ([`CLAMP_BACK_S`]).

use std::collections::VecDeque;

/// Seconds one bucket covers.
pub const BUCKET_S: i64 = 10;
/// Milliseconds one bucket covers.
pub const BUCKET_MS: i64 = BUCKET_S * 1000;
/// Buckets a ring keeps: an hour, what the lens shows.
pub const RING_BUCKETS: usize = 360;
/// The window "now" averages over, ms.
pub const NOW_WINDOW_MS: i64 = 10_000;
/// The longest step back that still lands in the open bucket, seconds.
pub const CLAMP_BACK_S: i64 = 60;
/// How long [`Recent`] keeps a reading, ms: the longest window asked of it.
pub const RECENT_KEEP_MS: i64 = 60_000;
const RECENT_CAP: usize = 256;

/// One bucket's readings, raw.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bucket {
    /// The readings' sum.
    pub sum: f64,
    /// How many readings; 0 is not read.
    pub n: u16,
    /// The largest reading (meaningless while `n` is 0).
    pub max: f64,
    /// How many readings were over the resource's maximum.
    pub over_n: u16,
}

impl Bucket {
    /// The mean reading; `None` when nothing was read.
    #[inline]
    #[must_use]
    pub fn value(&self) -> Option<f64> {
        (self.n > 0).then(|| self.sum / f64::from(self.n))
    }

    /// Over for at least half its readings.
    #[inline]
    #[must_use]
    pub const fn is_over(&self) -> bool {
        self.n > 0 && (self.over_n as u32) * 2 >= (self.n as u32)
    }

    /// The bucket as a strip cell.
    #[inline]
    #[must_use]
    pub fn cell(&self) -> Cell {
        Cell {
            mean: self.value(),
            max: (self.n > 0).then_some(self.max),
            over: self.is_over(),
        }
    }

    #[inline]
    fn add(&mut self, v: f64, over: bool) {
        self.max = if self.n == 0 { v } else { self.max.max(v) };
        self.sum += v;
        self.n = self.n.saturating_add(1);
        if over {
            self.over_n = self.over_n.saturating_add(1);
        }
    }
}

/// One strip cell: a bucket, or a column of consecutive buckets.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cell {
    /// The mean of its read buckets, raw; `None` when none was read (hollow).
    pub mean: Option<f64>,
    /// The largest single reading, for the lens and the accessibility text.
    pub max: Option<f64>,
    /// Whether any of its buckets was over for at least half its readings.
    pub over: bool,
}

impl Cell {
    /// Merge consecutive cells into one column.
    #[must_use]
    pub fn merge(cells: impl IntoIterator<Item = Cell>) -> Cell {
        let mut sum = 0.0;
        let mut n = 0u32;
        let mut out = Cell::default();
        for c in cells {
            if let Some(m) = c.mean {
                sum += m;
                n += 1;
            }
            if let Some(x) = c.max {
                out.max = Some(out.max.map_or(x, |o| o.max(x)));
            }
            out.over |= c.over;
        }
        out.mean = (n > 0).then(|| sum / f64::from(n));
        out
    }
}

/// One reading as it was taken: when, its value, and whether it was over the resource's maximum.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reading {
    /// When it was taken, unix ms.
    pub at_ms: i64,
    /// Its value, raw.
    pub v: f64,
    /// Whether it was over the resource's maximum.
    pub over: bool,
}

impl Reading {
    /// The reading as a strip cell of its own.
    #[inline]
    #[must_use]
    pub const fn cell(self) -> Cell {
        Cell {
            mean: Some(self.v),
            max: Some(self.v),
            over: self.over,
        }
    }
}

/// The last minute of readings with their wall-clock times, for a trailing mean ([`Recent::mean`])
/// and a strip drawn reading by reading ([`slots_of`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recent {
    v: VecDeque<Reading>,
}

impl Recent {
    /// Record reading `v` taken at `at_ms` (unix ms).
    pub fn push(&mut self, at_ms: i64, v: f64) {
        self.push_reading(Reading {
            at_ms,
            v,
            over: false,
        });
    }

    /// Record reading `r`; a reading not after the newest kept replaces the readings since it.
    pub fn push_reading(&mut self, r: Reading) {
        if !r.v.is_finite() {
            return;
        }
        while self.v.back().is_some_and(|b| b.at_ms > r.at_ms) {
            self.v.pop_back();
        }
        self.v.push_back(r);
        while self.v.front().is_some_and(|f| {
            i128::from(f.at_ms) <= i128::from(r.at_ms) - i128::from(RECENT_KEEP_MS)
        }) || self.v.len() > RECENT_CAP
        {
            self.v.pop_front();
        }
    }

    /// Mean of readings taken after `now_ms - window_ms` through `now_ms`.
    #[must_use]
    pub fn mean(&self, now_ms: i64, window_ms: i64) -> Option<f64> {
        let cutoff = i128::from(now_ms) - i128::from(window_ms);
        let (sum, n) = self
            .v
            .iter()
            .filter(|r| i128::from(r.at_ms) > cutoff && r.at_ms <= now_ms)
            .fold((0.0, 0u32), |(s, n), r| (s + r.v, n + 1));
        (n > 0).then(|| sum / f64::from(n))
    }

    /// Every reading kept, oldest first.
    #[must_use]
    pub fn readings(&self) -> Vec<Reading> {
        self.v.iter().copied().collect()
    }

    /// Forget every reading.
    pub fn clear(&mut self) {
        self.v.clear();
    }

    fn shift(&mut self, by_ms: i128) {
        self.v.retain_mut(|r| {
            let Ok(shifted) = i64::try_from(i128::from(r.at_ms) + by_ms) else {
                return false;
            };
            r.at_ms = shifted;
            true
        });
    }
}

/// How long a reading slot is, ms: a second, the focused window's cadence.
pub const SLOT_MS: i64 = 1000;

/// How close to a slot's edge a reading may land before [`SlotGrid::clear_of`] moves the edges,
/// ms: a tenth of a second, under a quarter of the lens's half-second cadence, so its two readings
/// a second both stand clear.
pub const SLOT_GUARD_MS: i64 = 100;

/// Where reading slots stand on the wall clock: `slot_ms` long, each starting `origin_ms` past a
/// multiple of `slot_ms`. A strip of slots draws a reading a slot, so its edges must stand
/// between the readings, never on them: readings paced from the moment each was asked drift
/// through the second, and a reading on a slot's edge would land in the slot before or after by
/// a millisecond, leaving one slot empty and the next holding two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotGrid {
    /// A slot's length, ms.
    pub slot_ms: i64,
    /// Where in each `slot_ms` a slot starts, ms (`0..slot_ms`).
    pub origin_ms: i64,
}

impl Default for SlotGrid {
    fn default() -> Self {
        Self::SECONDS
    }
}

impl SlotGrid {
    /// The wall clock's own seconds.
    pub const SECONDS: Self = Self {
        slot_ms: SLOT_MS,
        origin_ms: 0,
    };

    const fn len(self) -> i64 {
        if self.slot_ms > 0 { self.slot_ms } else { 1 }
    }

    /// The index of the slot holding `at_ms`.
    #[must_use]
    pub const fn slot(self, at_ms: i64) -> i64 {
        at_ms.saturating_sub(self.origin_ms).div_euclid(self.len())
    }

    /// How many slots a reading at `at_ms` opened past one at `prev_ms` (0 for one in the same
    /// slot, negative for one before it).
    #[must_use]
    pub const fn opened(self, prev_ms: i64, at_ms: i64) -> i64 {
        self.slot(at_ms).saturating_sub(self.slot(prev_ms))
    }

    /// Whether `at_ms` stands at least `guard_ms` inside its slot, clear of both edges.
    #[must_use]
    pub const fn clear(self, at_ms: i64, guard_ms: i64) -> bool {
        let into = at_ms.saturating_sub(self.origin_ms).rem_euclid(self.len());
        into >= guard_ms && self.len() - into >= guard_ms
    }

    /// The grid of `slot_ms` whose edges stand furthest from every one of `times` (unix ms):
    /// each edge in the middle of the widest gap between the times' places in a slot. Readings
    /// a second apart then stand in the middles of their slots, and the lens's two a second a
    /// quarter of a slot inside each edge. With no times, the wall clock's own seconds.
    #[must_use]
    pub fn clear_of(times: &[i64], slot_ms: i64) -> Self {
        let len = slot_ms.max(1);
        let mut phases: Vec<i64> = times.iter().map(|t| t.rem_euclid(len)).collect();
        phases.sort_unstable();
        phases.dedup();
        let (Some(&first), Some(&last)) = (phases.first(), phases.last()) else {
            return Self {
                slot_ms: len,
                origin_ms: 0,
            };
        };
        let mut widest = (first + len - last, last);
        for pair in phases.windows(2) {
            let gap = pair[1] - pair[0];
            if gap > widest.0 {
                widest = (gap, pair[0]);
            }
        }
        Self {
            slot_ms: len,
            origin_ms: (widest.1 + widest.0 / 2).rem_euclid(len),
        }
    }
}

/// `n` slots of `grid`, oldest first, the last the one holding `at_ms` (the snapshot's reading):
/// each the merge of the `readings` taken in it ([`Cell::merge`]). A slot no reading fell in
/// takes the first reading after it, which speaks for the time back to the reading before (a
/// rate read at `t` is the mean since that reading), when that span is at most `cover_ms`; else
/// it is unread, as is the last slot of a series not read at `at_ms`. `readings` are oldest
/// first, as [`Recent::readings`] keeps them.
#[must_use]
pub fn slots_of(
    readings: &[Reading],
    at_ms: i64,
    grid: SlotGrid,
    n: usize,
    cover_ms: i64,
) -> Vec<Cell> {
    let slot_ms = grid.len();
    let readings = &readings[..readings.partition_point(|r| r.at_ms <= at_ms)];
    let last = grid.slot(at_ms);
    (0..n)
        .map(|j| {
            let k = i128::from(last) - (n - 1 - j) as i128;
            let lo = k * i128::from(slot_ms) + i128::from(grid.origin_ms);
            let hi = lo + i128::from(slot_ms);
            let from = readings.partition_point(|r| i128::from(r.at_ms) < lo);
            let to = readings.partition_point(|r| i128::from(r.at_ms) < hi);
            let inside = readings.get(from..to).unwrap_or_default();
            if !inside.is_empty() {
                return Cell::merge(inside.iter().map(|r| r.cell()));
            }
            match (
                to.checked_sub(1).and_then(|i| readings.get(i)),
                readings.get(to),
            ) {
                (Some(before), Some(after))
                    if i128::from(after.at_ms) - i128::from(before.at_ms)
                        <= i128::from(cover_ms) =>
                {
                    after.cell()
                }
                _ => Cell::default(),
            }
        })
        .collect()
}

/// An hour of 10 s buckets, keyed by the wall clock.
#[derive(Clone, Debug, PartialEq)]
pub struct BucketRing {
    cap: usize,
    /// The key of `b[0]`.
    first_k: i64,
    b: VecDeque<Bucket>,
    /// The readings behind "now".
    recent: Recent,
}

impl Default for BucketRing {
    fn default() -> Self {
        Self::with_capacity(RING_BUCKETS)
    }
}

impl BucketRing {
    /// An empty ring of at most `cap` buckets (at least 1).
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        let cap = cap.max(1);
        Self {
            cap,
            first_k: 0,
            b: VecDeque::with_capacity(cap),
            recent: Recent::default(),
        }
    }

    /// Record a reading taken at `at_unix_ms`.
    pub fn push(&mut self, at_unix_ms: i64, v: Option<f64>, over: bool) -> Option<i64> {
        let v = v.filter(|x| x.is_finite());
        let k = at_unix_ms.div_euclid(BUCKET_MS);
        let closed = match self.open_k() {
            None => {
                self.first_k = k;
                self.b.push_back(Bucket::default());
                None
            }
            Some(open) if k == open => None,
            Some(open) if k > open => {
                if k - open >= self.cap as i64 {
                    self.b.clear();
                    self.first_k = k;
                } else {
                    for _ in 1..k - open {
                        self.b.push_back(Bucket::default());
                    }
                }
                self.b.push_back(Bucket::default());
                self.trim();
                Some(open)
            }
            Some(open) if (open - k) * BUCKET_S <= CLAMP_BACK_S => None,
            Some(open) => {
                let step = open + 1 - k;
                self.first_k -= step;
                self.recent.shift(-i128::from(step) * i128::from(BUCKET_MS));
                self.b.push_back(Bucket::default());
                self.trim();
                Some(open - step)
            }
        };
        if let Some(v) = v {
            if let Some(b) = self.b.back_mut() {
                b.add(v, over);
            }
            self.recent.push_reading(Reading {
                at_ms: at_unix_ms,
                v,
                over,
            });
        }
        closed
    }

    fn trim(&mut self) {
        while self.b.len() > self.cap {
            self.b.pop_front();
            self.first_k += 1;
        }
    }

    /// The open bucket's key; `None` before the first push.
    #[inline]
    #[must_use]
    pub fn open_k(&self) -> Option<i64> {
        (!self.b.is_empty()).then(|| self.first_k + self.b.len() as i64 - 1)
    }

    /// The bucket with key `k`, while the ring holds it.
    #[inline]
    #[must_use]
    pub fn bucket(&self, k: i64) -> Option<&Bucket> {
        let i = usize::try_from(k.checked_sub(self.first_k)?).ok()?;
        self.b.get(i)
    }

    /// Every bucket with its key, oldest first.
    #[inline]
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (i64, &Bucket)> {
        let first = self.first_k;
        self.b
            .iter()
            .enumerate()
            .map(move |(i, b)| (first + i as i64, b))
    }

    /// How many buckets it holds (unread ones included).
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.b.len()
    }

    /// Whether nothing was pushed yet.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.b.is_empty()
    }

    /// Every bucket as a cell, oldest first; the last is the open bucket.
    #[must_use]
    pub fn cells(&self) -> Vec<Cell> {
        self.b.iter().map(Bucket::cell).collect()
    }

    /// The newest `n` buckets as cells, oldest first; the last is the open bucket.
    #[must_use]
    pub fn tail_cells(&self, n: usize) -> Vec<Cell> {
        self.b
            .iter()
            .skip(self.b.len().saturating_sub(n))
            .map(Bucket::cell)
            .collect()
    }

    /// Computes `ncols` wall-clock-aligned columns of `per_s` seconds ending at `now_unix_ms`.
    #[must_use]
    pub fn columns(&self, per_s: u32, ncols: usize, now_unix_ms: i64) -> Vec<Cell> {
        columns_of(
            &self.cells(),
            self.open_k().unwrap_or(0),
            per_s,
            ncols,
            now_unix_ms,
        )
    }

    /// The last minute of readings, oldest first ([`slots_of`]).
    #[must_use]
    pub fn readings(&self) -> Vec<Reading> {
        self.recent.readings()
    }

    /// Mean of readings in the `window_ms` preceding `now_unix_ms` ([`NOW_WINDOW_MS`]).
    #[must_use]
    pub fn now_mean(&self, now_unix_ms: i64, window_ms: i64) -> Option<f64> {
        self.recent.mean(now_unix_ms, window_ms)
    }

    /// Whether the open bucket is over.
    #[must_use]
    pub fn open_over(&self) -> bool {
        self.b.back().is_some_and(Bucket::is_over)
    }

    /// The sustained-30 s value ending at bucket `k`.
    #[must_use]
    pub fn sustained(&self, k: i64) -> Option<f64> {
        (0..3).try_fold(f64::INFINITY, |m, i| {
            self.bucket(k - i).and_then(Bucket::value).map(|v| m.min(v))
        })
    }

    /// Compact snapshot of non-empty 10 s buckets and last-minute readings for persistence.
    #[must_use]
    pub fn saved(&self) -> SavedRing {
        let buckets = self
            .iter()
            .filter_map(|(k, b)| {
                (b.n > 0 && b.sum.is_finite() && b.max.is_finite())
                    .then_some((k, b.sum, b.n, b.max, b.over_n))
            })
            .collect();
        let recent = self
            .recent
            .readings()
            .into_iter()
            .filter(|r| r.v.is_finite())
            .map(|r| (r.at_ms, r.v, r.over))
            .collect();
        SavedRing { buckets, recent }
    }

    /// Restore a ring persisted in an earlier run up to `now_unix_ms`, pruning stale or future
    /// entries outside the 1-hour window.
    #[must_use]
    pub fn restored(saved: &SavedRing, now_unix_ms: i64) -> Self {
        let mut ring = Self::default();
        if now_unix_ms <= 0 {
            return ring;
        }
        let now_k = now_unix_ms.div_euclid(BUCKET_MS);
        let min_k = now_k - (RING_BUCKETS as i64 - 1);
        let mut valid: Vec<SavedBucket> = saved
            .buckets
            .iter()
            .copied()
            .filter(|&(k, sum, n, max, _)| {
                n > 0 && sum.is_finite() && max.is_finite() && k >= min_k && k <= now_k
            })
            .collect();
        valid.sort_by_key(|&(k, ..)| k);
        valid.dedup_by_key(|&mut (k, ..)| k);
        if let (Some(&(first_k, ..)), Some(&(last_k, ..))) = (valid.first(), valid.last()) {
            let len = usize::try_from(last_k - first_k + 1)
                .unwrap_or(0)
                .min(RING_BUCKETS);
            ring.first_k = last_k - len as i64 + 1;
            ring.b.resize(len, Bucket::default());
            for (k, sum, n, max, over_n) in valid {
                if let Ok(idx) = usize::try_from(k - ring.first_k)
                    && let Some(slot) = ring.b.get_mut(idx)
                {
                    *slot = Bucket {
                        sum,
                        n,
                        max,
                        over_n: over_n.min(n),
                    };
                }
            }
        }
        let min_recent_ms = now_unix_ms.saturating_sub(RECENT_KEEP_MS);
        let mut recs: Vec<SavedReading> = saved
            .recent
            .iter()
            .copied()
            .filter(|&(at_ms, v, _)| v.is_finite() && at_ms > min_recent_ms && at_ms <= now_unix_ms)
            .collect();
        recs.sort_by_key(|&(at_ms, ..)| at_ms);
        for (at_ms, v, over) in recs {
            ring.recent.push_reading(Reading { at_ms, v, over });
        }
        ring
    }
}

/// One persisted non-empty 10 s bucket `(k, sum, n, max, over_n)`.
pub type SavedBucket = (i64, f64, u16, f64, u16);

/// One persisted per-sample reading `(at_ms, value, over)`.
pub type SavedReading = (i64, f64, bool);

/// Compact persisted state of one [`BucketRing`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavedRing {
    /// Non-empty 10 s buckets within the hour, oldest first.
    pub buckets: Vec<SavedBucket>,
    /// Per-sample readings within the last minute, oldest first.
    pub recent: Vec<SavedReading>,
}

impl SavedRing {
    /// True when neither buckets nor recent readings are held.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty() && self.recent.is_empty()
    }
}

/// Computes [`BucketRing::columns`] over a snapshot's `cells` ending at `last_k`.
#[must_use]
pub fn columns_of(
    cells: &[Cell],
    last_k: i64,
    per_s: u32,
    ncols: usize,
    now_unix_ms: i64,
) -> Vec<Cell> {
    let per_b = i64::from((per_s / 10).max(1));
    let now_k = now_unix_ms.div_euclid(BUCKET_MS);
    let last_start = now_k.div_euclid(per_b) * per_b;
    let first_k = last_k + 1 - cells.len() as i64;
    let at = |k: i64| -> Option<Cell> {
        (k >= first_k && k <= last_k && k <= now_k)
            .then(|| usize::try_from(k - first_k).ok())
            .flatten()
            .and_then(|i| cells.get(i).copied())
    };
    (0..ncols)
        .map(|j| {
            let start = last_start - (ncols - 1 - j) as i64 * per_b;
            Cell::merge((start..start + per_b).filter_map(at))
        })
        .collect()
}

/// Reason a wall-clock span on a time-series strip holds no continuous live reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GapKind {
    /// Sampling paused or stalled past the continuity threshold while awake.
    Paused,
    /// System sleep (`mono_ms` paused while the wall clock advanced, or OS sleep/wake window).
    Sleep,
    /// Unrecorded span across an abnormal termination or crash before restart.
    Crash,
}

/// Half-open wall-clock discontinuity `[start_ms, end_ms)` in Unix milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TimeGap {
    /// Inclusive start of the discontinuity (Unix ms).
    pub start_ms: i64,
    /// Exclusive end of the discontinuity (Unix ms).
    pub end_ms: i64,
    /// Kind of discontinuity.
    pub kind: GapKind,
}

impl TimeGap {
    /// Overlap in milliseconds between `[self.start_ms, self.end_ms)` and `[lo_ms, hi_ms)`.
    #[inline]
    #[must_use]
    pub fn overlap_ms(&self, lo_ms: i64, hi_ms: i64) -> i64 {
        self.end_ms
            .min(hi_ms)
            .saturating_sub(self.start_ms.max(lo_ms))
            .max(0)
    }
}

/// One resolution band in a [`MultiScaleSpec`], ordered oldest (left) to newest (right).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScaleBand {
    /// Number of consecutive boxes in this band.
    pub slots: u16,
    /// Wall-clock duration each box in this band covers, in milliseconds.
    pub period_ms: i64,
    /// Relative horizontal width weight of each box in this band.
    pub weight: u16,
    /// True for the far-right instantaneous live `now` box.
    pub live_now: bool,
}

/// Normalized geometry and age window of one box in a [`MultiScaleSpec`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProgSlot {
    /// Normalized left edge in `0.0..=1.0` across the strip.
    pub x0: f32,
    /// Normalized right edge in `0.0..=1.0` across the strip.
    pub x1: f32,
    /// Older edge of the slot's window relative to `now_unix_ms` (ms ago, `>= age_lo_ms`).
    pub age_hi_ms: i64,
    /// Newer edge of the slot's window relative to `now_unix_ms` (ms ago, `0` on `live_now`).
    pub age_lo_ms: i64,
    /// Relative width weight from its [`ScaleBand`].
    pub weight: u16,
    /// True when this slot is the far-right glowing real-time `now` box.
    pub is_live_now: bool,
}

/// One X-axis tick mark and horizon label on a [`MultiScaleSpec`] strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisTick {
    /// Normalized horizontal coordinate in `0.0..=1.0`.
    pub x_frac: f32,
    /// Age at this tick in milliseconds before `now` (`0` for `"now"`).
    pub age_ms: i64,
    /// Horizon label (`"–1h"`, `"–15m"`, `"–5m"`, `"–1m"`, `"–10s"`, `"now"`).
    pub label: &'static str,
    /// True when the label anchors to the right edge (`"now"`).
    pub align_right: bool,
}

/// One resampled cell in a [`MultiScaleSpec`] strip, carrying its slot geometry and gap marker.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScaleCell {
    /// Aggregated reading values in this slot (`None` mean when hollow or gap-cleared).
    pub cell: Cell,
    /// Discontinuity kind when this slot represents a sleep, crash, or pause gap.
    pub gap: Option<GapKind>,
    /// Normalized slot geometry and age bounds.
    pub slot: ProgSlot,
}

/// Multi-scale progressive time-series specification: fine real-time boxes near `now` on the
/// right growing into wider aggregated buckets toward the left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MultiScaleSpec {
    /// Resolution bands ordered oldest (left) to newest (right).
    pub bands: &'static [ScaleBand],
    /// Horizon ticks `(age_ms, label)` ordered left to right.
    pub ticks: &'static [(i64, &'static str)],
}

const BANDS_FULL_HOUR: [ScaleBand; 6] = [
    ScaleBand {
        slots: 3,
        period_ms: 900_000,
        weight: 16,
        live_now: false,
    },
    ScaleBand {
        slots: 2,
        period_ms: 300_000,
        weight: 13,
        live_now: false,
    },
    ScaleBand {
        slots: 4,
        period_ms: 60_000,
        weight: 10,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 10_000,
        weight: 7,
        live_now: false,
    },
    ScaleBand {
        slots: 4,
        period_ms: 2_000,
        weight: 5,
        live_now: false,
    },
    ScaleBand {
        slots: 1,
        period_ms: 2_000,
        weight: 11,
        live_now: true,
    },
];

const TICKS_FULL_HOUR: [(i64, &str); 6] = [
    (3_600_000, "\u{2013}1h"),
    (900_000, "\u{2013}15m"),
    (300_000, "\u{2013}5m"),
    (60_000, "\u{2013}1m"),
    (10_000, "\u{2013}10s"),
    (0, "now"),
];

const BANDS_FOLD_15M: [ScaleBand; 5] = [
    ScaleBand {
        slots: 2,
        period_ms: 300_000,
        weight: 15,
        live_now: false,
    },
    ScaleBand {
        slots: 4,
        period_ms: 60_000,
        weight: 11,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 10_000,
        weight: 7,
        live_now: false,
    },
    ScaleBand {
        slots: 4,
        period_ms: 2_000,
        weight: 5,
        live_now: false,
    },
    ScaleBand {
        slots: 1,
        period_ms: 2_000,
        weight: 11,
        live_now: true,
    },
];

const TICKS_FOLD_15M: [(i64, &str); 5] = [
    (900_000, "\u{2013}15m"),
    (300_000, "\u{2013}5m"),
    (60_000, "\u{2013}1m"),
    (10_000, "\u{2013}10s"),
    (0, "now"),
];

const BANDS_COMPACT_5M: [ScaleBand; 4] = [
    ScaleBand {
        slots: 2,
        period_ms: 120_000,
        weight: 14,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 10_000,
        weight: 8,
        live_now: false,
    },
    ScaleBand {
        slots: 4,
        period_ms: 2_000,
        weight: 5,
        live_now: false,
    },
    ScaleBand {
        slots: 1,
        period_ms: 2_000,
        weight: 11,
        live_now: true,
    },
];

const TICKS_COMPACT_5M: [(i64, &str); 4] = [
    (300_000, "\u{2013}5m"),
    (60_000, "\u{2013}1m"),
    (10_000, "\u{2013}10s"),
    (0, "now"),
];

const BANDS_MINI_1M: [ScaleBand; 4] = [
    ScaleBand {
        slots: 2,
        period_ms: 20_000,
        weight: 13,
        live_now: false,
    },
    ScaleBand {
        slots: 1,
        period_ms: 10_000,
        weight: 8,
        live_now: false,
    },
    ScaleBand {
        slots: 4,
        period_ms: 2_000,
        weight: 4,
        live_now: false,
    },
    ScaleBand {
        slots: 1,
        period_ms: 2_000,
        weight: 9,
        live_now: true,
    },
];

const TICKS_MINI_1M: [(i64, &str); 3] =
    [(60_000, "\u{2013}1m"), (10_000, "\u{2013}10s"), (0, "now")];

impl MultiScaleSpec {
    /// Full 1-hour multi-scale horizon (`15m → 5m → 1m → 10s → 2s → now`, 19 slots).
    pub const FULL_HOUR: Self = Self {
        bands: &BANDS_FULL_HOUR,
        ticks: &TICKS_FULL_HOUR,
    };

    /// 15-minute multi-scale horizon (`5m → 1m → 10s → 2s → now`, 16 slots).
    pub const FOLD_15M: Self = Self {
        bands: &BANDS_FOLD_15M,
        ticks: &TICKS_FOLD_15M,
    };

    /// Compact 5-minute multi-scale horizon (`2m → 10s → 2s → now`, 12 slots).
    pub const COMPACT_5M: Self = Self {
        bands: &BANDS_COMPACT_5M,
        ticks: &TICKS_COMPACT_5M,
    };

    /// Narrow 1-minute multi-scale horizon (`20s → 10s → 2s → now`, 8 slots).
    pub const MINI_1M: Self = Self {
        bands: &BANDS_MINI_1M,
        ticks: &TICKS_MINI_1M,
    };

    /// Pick the widest-horizon multi-scale preset whose finest (`2s`) boxes remain crisp inside
    /// `strip_px` device pixels at `scale_x100` (`100` = 1x, `200` = 2x).
    #[must_use]
    pub fn for_strip_px(strip_px: u32, scale_x100: u32, prefer_hour: bool) -> Self {
        let pt = strip_px.saturating_mul(100) / scale_x100.max(1);
        if prefer_hour && pt >= 156 {
            Self::FULL_HOUR
        } else if pt >= 120 {
            if prefer_hour && pt >= 140 {
                Self::FULL_HOUR
            } else {
                Self::FOLD_15M
            }
        } else if pt >= 82 {
            Self::COMPACT_5M
        } else {
            Self::MINI_1M
        }
    }

    /// Total number of boxes across all bands.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.bands.iter().map(|b| usize::from(b.slots)).sum()
    }

    /// Total wall-clock horizon covered by the strip, in milliseconds.
    #[must_use]
    pub fn horizon_ms(&self) -> i64 {
        self.bands
            .iter()
            .map(|b| i64::from(b.slots) * b.period_ms)
            .sum()
    }

    /// Total horizontal weight across all boxes.
    #[must_use]
    pub fn total_weight(&self) -> u32 {
        self.bands
            .iter()
            .map(|b| u32::from(b.slots) * u32::from(b.weight))
            .sum::<u32>()
            .max(1)
    }

    /// Compute normalized `[x0, x1]` and `[age_lo_ms, age_hi_ms]` layout for every slot, oldest first.
    #[must_use]
    pub fn slots(&self) -> Vec<ProgSlot> {
        let total_w = self.total_weight() as f32;
        let mut rem_age = self.horizon_ms();
        let mut acc_w = 0u32;
        let mut out = Vec::with_capacity(self.slot_count());
        for band in self.bands {
            for _ in 0..band.slots {
                let next_w = acc_w + u32::from(band.weight);
                let next_age = (rem_age - band.period_ms).max(0);
                out.push(ProgSlot {
                    x0: acc_w as f32 / total_w,
                    x1: next_w as f32 / total_w,
                    age_hi_ms: rem_age,
                    age_lo_ms: next_age,
                    weight: band.weight,
                    is_live_now: band.live_now,
                });
                acc_w = next_w;
                rem_age = next_age;
            }
        }
        out
    }

    /// Map an event age `age_ms` (`now_unix_ms - event_unix_ms`) to its exact continuous
    /// normalized X coordinate in `0.0..=1.0`, or `None` when older than [`Self::horizon_ms`].
    #[must_use]
    pub fn x_frac_of_age_ms(&self, age_ms: i64) -> Option<f32> {
        if age_ms < -2_000 {
            return None;
        }
        let age = age_ms.max(0);
        let horizon = self.horizon_ms();
        if age > horizon {
            return None;
        }
        for slot in self.slots() {
            if age <= slot.age_hi_ms && age >= slot.age_lo_ms {
                let span = (slot.age_hi_ms - slot.age_lo_ms).max(1) as f32;
                let into = (slot.age_hi_ms - age) as f32 / span;
                return Some((slot.x0 + (slot.x1 - slot.x0) * into).clamp(0.0, 1.0));
            }
        }
        Some(1.0)
    }

    /// Horizon ticks with exact normalized X positions on this strip.
    #[must_use]
    pub fn axis_ticks(&self) -> Vec<AxisTick> {
        self.ticks
            .iter()
            .filter_map(|&(age_ms, label)| {
                let x_frac = if age_ms == 0 {
                    1.0
                } else {
                    self.x_frac_of_age_ms(age_ms)?
                };
                Some(AxisTick {
                    x_frac,
                    age_ms,
                    label,
                    align_right: age_ms == 0,
                })
            })
            .collect()
    }
}

fn slot_gap_kind(
    gaps: &[TimeGap],
    lo_ms: i64,
    hi_ms: i64,
    has_reading: bool,
) -> (Option<GapKind>, bool) {
    let dur = hi_ms.saturating_sub(lo_ms).max(1);
    let mut best: Option<GapKind> = None;
    let mut total_ov = 0i64;
    for g in gaps {
        let ov = g.overlap_ms(lo_ms, hi_ms);
        if ov > 0 {
            total_ov = total_ov.saturating_add(ov);
            best = Some(best.map_or(g.kind, |b| b.max(g.kind)));
        }
    }
    let Some(kind) = best else {
        return (None, false);
    };
    let dominates = total_ov.saturating_mul(2) >= dur;
    if !has_reading || dominates {
        (Some(kind), dominates)
    } else {
        (None, false)
    }
}

/// Resample a 1-hour bucket ring (`ring_cells` ending at `last_k`) and last-minute per-sample
/// `recent` readings into progressive [`ScaleCell`]s for `spec` at `now_unix_ms`, marking
/// discontinuous `gaps` without smearing adjacent readings across them.
#[must_use]
pub fn multi_scale_cells(
    ring_cells: &[Cell],
    last_k: i64,
    recent: &[Reading],
    now_unix_ms: i64,
    spec: MultiScaleSpec,
    cover_ms: i64,
    gaps: &[TimeGap],
) -> Vec<ScaleCell> {
    let slots = spec.slots();
    let now_k = now_unix_ms.div_euclid(BUCKET_MS);
    let first_k = last_k + 1 - ring_cells.len() as i64;
    let ring_at = |k: i64| -> Option<Cell> {
        (k >= first_k && k <= last_k && k <= now_k)
            .then(|| usize::try_from(k - first_k).ok())
            .flatten()
            .and_then(|i| ring_cells.get(i).copied())
    };
    let recent_valid = &recent[..recent.partition_point(|r| r.at_ms <= now_unix_ms)];
    slots
        .into_iter()
        .map(|slot| {
            let lo_ms = now_unix_ms.saturating_sub(slot.age_hi_ms);
            let hi_ms = now_unix_ms.saturating_sub(slot.age_lo_ms);
            let any_gap = gaps.iter().any(|g| g.overlap_ms(lo_ms, hi_ms) > 0);
            let ring_cell = || {
                let k_lo = lo_ms.div_euclid(BUCKET_MS);
                let k_hi = hi_ms.saturating_sub(1).max(lo_ms).div_euclid(BUCKET_MS);
                Cell::merge((k_lo..=k_hi).filter_map(ring_at))
            };
            let mut raw = if slot.age_hi_ms <= RECENT_KEEP_MS && !recent_valid.is_empty() {
                let from = recent_valid.partition_point(|r| r.at_ms < lo_ms);
                let to = if slot.is_live_now {
                    recent_valid.partition_point(|r| r.at_ms <= hi_ms)
                } else {
                    recent_valid.partition_point(|r| r.at_ms < hi_ms)
                };
                let inside = recent_valid.get(from..to).unwrap_or_default();
                if !inside.is_empty() {
                    Cell::merge(inside.iter().map(|r| r.cell()))
                } else if to == 0 && slot.age_hi_ms - slot.age_lo_ms >= BUCKET_MS {
                    ring_cell()
                } else if !any_gap {
                    match (
                        to.checked_sub(1).and_then(|i| recent_valid.get(i)),
                        recent_valid.get(to),
                    ) {
                        (Some(before), Some(after))
                            if i128::from(after.at_ms) - i128::from(before.at_ms)
                                <= i128::from(cover_ms) =>
                        {
                            after.cell()
                        }
                        (Some(before), None)
                            if slot.is_live_now
                                && i128::from(now_unix_ms) - i128::from(before.at_ms)
                                    <= i128::from(cover_ms) =>
                        {
                            before.cell()
                        }
                        _ => Cell::default(),
                    }
                } else {
                    Cell::default()
                }
            } else {
                ring_cell()
            };
            let live_resumed = slot.is_live_now
                && raw.mean.is_some()
                && recent_valid.last().is_some_and(|newest| {
                    gaps.iter()
                        .all(|g| g.overlap_ms(lo_ms, hi_ms) == 0 || newest.at_ms >= g.end_ms)
                });
            let (gap, clear_reading) = if live_resumed {
                (None, false)
            } else {
                slot_gap_kind(gaps, lo_ms, hi_ms, raw.mean.is_some())
            };
            if clear_reading {
                raw = Cell::default();
            }
            ScaleCell {
                cell: raw,
                gap,
                slot,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_792_000_800_000;

    #[test]
    fn buckets_close_by_wall_clock() {
        let mut r = BucketRing::default();
        assert_eq!(r.push(T0 + 9_900, Some(2.0), false), None);
        assert_eq!(
            r.push(T0 + 10_000, Some(4.0), false),
            Some(T0 / BUCKET_MS),
            "a reading in the next bucket closes the open one"
        );
        assert_eq!(r.bucket(T0 / BUCKET_MS).and_then(Bucket::value), Some(2.0));
        assert_eq!(
            r.bucket(T0 / BUCKET_MS + 1).and_then(Bucket::value),
            Some(4.0)
        );
        let _ = r.push(T0 + 30_500, Some(6.0), false);
        let cells = r.cells();
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[2], Cell::default(), "not read is None, never 0");
        assert_eq!(cells[3].mean, Some(6.0));
        let _ = r.push(T0 + 40_000, None, false);
        assert_eq!(r.cells()[4].mean, None);
    }

    #[test]
    fn the_tail_is_the_newest_buckets_with_the_open_one_last() {
        let mut r = BucketRing::default();
        assert!(r.tail_cells(3).is_empty());
        for i in [0, 1, 3, 4] {
            let _ = r.push(T0 + i * 10_000, Some(i as f64), false);
        }
        let means = |cells: Vec<Cell>| cells.into_iter().map(|c| c.mean).collect::<Vec<_>>();
        assert_eq!(
            means(r.tail_cells(3)),
            [None, Some(3.0), Some(4.0)],
            "a gap bucket is unread, never 0"
        );
        assert_eq!(
            r.tail_cells(99),
            r.cells(),
            "asking past its length gives all"
        );
        assert!(r.tail_cells(0).is_empty());
        let _ = r.push(T0 + 45_000, Some(8.0), false);
        assert_eq!(
            means(r.tail_cells(1)),
            [Some(6.0)],
            "the open bucket is last"
        );
    }

    #[test]
    fn columns_merge_on_wall_clock_multiples() {
        let mut r = BucketRing::default();
        for i in 0..10 {
            let v = (i != 4).then_some(f64::from(i));
            let _ = r.push(T0 + i64::from(i) * 10_000, v, i == 7);
        }
        let now = T0 + 95_000;
        let ten = r.columns(10, 3, now);
        assert_eq!(
            ten.iter().map(|c| c.mean).collect::<Vec<_>>(),
            [Some(7.0), Some(8.0), Some(9.0)]
        );
        let twenty = r.columns(20, 3, now);
        assert_eq!(twenty[2].mean, Some(8.5));
        assert_eq!(twenty[1].mean, Some(6.5));
        assert!(twenty[1].over, "a column is over when one bucket is");
        assert_eq!(twenty[1].max, Some(7.0));
        assert_eq!(twenty[0].mean, Some(5.0), "the unread bucket is skipped");
        let thirty = r.columns(30, 5, now);
        assert_eq!(thirty[4].mean, Some(9.0));
        assert_eq!(thirty[3].mean, Some(7.0));
        assert_eq!(thirty[2].mean, Some(4.0));
        assert_eq!(thirty[1].mean, Some(1.0));
        assert_eq!(thirty[0], Cell::default());
        let mut sparse = BucketRing::default();
        let _ = sparse.push(T0, Some(1.0), false);
        let _ = sparse.push(T0 + 40_000, Some(1.0), false);
        let cols = sparse.columns(20, 3, T0 + 40_000);
        assert_eq!(cols[1].mean, None);
        assert_eq!(cols[0].mean, Some(1.0));
    }

    #[test]
    fn unknown_is_never_zero_in_a_bucket_a_column_or_now() {
        let mut r = BucketRing::default();
        for i in 0..6 {
            let _ = r.push(T0 + i * 10_000, None, false);
        }
        assert!(
            r.cells()
                .iter()
                .all(|c| c.mean.is_none() && c.max.is_none())
        );
        assert!(
            r.columns(30, 2, T0 + 55_000)
                .iter()
                .all(|c| c.mean.is_none())
        );
        assert_eq!(r.now_mean(T0 + 55_000, NOW_WINDOW_MS), None);
        assert_eq!(Bucket::default().value(), None);
        assert!(!Bucket::default().is_over());
        let _ = r.push(T0 + 60_000, Some(0.0), false);
        assert_eq!(r.now_mean(T0 + 60_000, NOW_WINDOW_MS), Some(0.0));
        assert_eq!(r.cells().last().and_then(|c| c.mean), Some(0.0));
    }

    #[test]
    fn clock_steps_back() {
        let mut r = BucketRing::default();
        for i in 0..6 {
            let _ = r.push(T0 + i * 10_000, Some(i as f64), false);
        }
        let open = r.open_k().expect("pushed");
        assert_eq!(r.push(T0 + 25_000, Some(5.0), false), None);
        assert_eq!(r.open_k(), Some(open));
        assert_eq!(r.bucket(open).map(|b| b.n), Some(2));
        let before: Vec<Cell> = r.cells();
        let back = T0 + 25_000 - 2 * 3_600_000;
        let closed = r.push(back, Some(9.0), false);
        let k = back.div_euclid(BUCKET_MS);
        assert_eq!(r.open_k(), Some(k));
        assert_eq!(closed, Some(k - 1));
        let after = r.cells();
        assert_eq!(&after[..before.len()], &before[..]);
        assert_eq!(after.last().and_then(|c| c.mean), Some(9.0));
        assert_eq!(r.now_mean(back, NOW_WINDOW_MS), Some(9.0));
        let _ = r.push(back + 40_000, Some(1.0), false);
        let cells = r.cells();
        let n = cells.len();
        assert_eq!(cells[n - 4..n - 1], [Cell::default(); 3]);
        assert_eq!(cells[n - 1].mean, Some(1.0));
        let mut extremes = BucketRing::default();
        let _ = extremes.push(i64::MAX, Some(1.0), false);
        let _ = extremes.push(i64::MIN, Some(2.0), false);
        assert_eq!(extremes.open_k(), Some(i64::MIN.div_euclid(BUCKET_MS)));
        assert_eq!(extremes.now_mean(i64::MIN, NOW_WINDOW_MS), Some(2.0));
    }

    #[test]
    fn the_ring_keeps_an_hour_and_a_long_silence_is_unread() {
        let mut r = BucketRing::default();
        for i in 0..400 {
            let _ = r.push(T0 + i * 10_000, Some(1.0), false);
        }
        assert_eq!(r.len(), RING_BUCKETS);
        assert_eq!(r.open_k(), Some(T0 / BUCKET_MS + 399));
        let _ = r.push(T0 + 399 * 10_000 + 2 * 3_600_000, Some(1.0), false);
        assert_eq!(r.len(), 1, "two silent hours leave nothing to show");
    }

    #[test]
    fn now_is_the_trailing_mean_whatever_the_bucket_boundary() {
        let mut edge = Recent::default();
        edge.push(i64::MIN, 4.0);
        assert_eq!(edge.mean(i64::MIN, NOW_WINDOW_MS), Some(4.0));
        assert_eq!(edge.mean(i64::MIN, 0), None);
        assert_eq!(edge.mean(i64::MIN, -1), None);
        edge.push(i64::MIN + RECENT_KEEP_MS, 8.0);
        assert_eq!(edge.mean(i64::MIN + RECENT_KEEP_MS, i64::MAX), Some(8.0));
        edge.push(i64::MAX, 16.0);
        assert_eq!(edge.mean(i64::MAX, NOW_WINDOW_MS), Some(16.0));
        assert_eq!(edge.mean(i64::MAX, i64::MIN), None);
        let mut r = BucketRing::default();
        for i in 0..15 {
            let _ = r.push(T0 + i * 1000, Some(i as f64), false);
        }
        assert_eq!(r.now_mean(T0 + 14_000, NOW_WINDOW_MS), Some(9.5));
        assert_eq!(
            r.bucket(T0 / BUCKET_MS + 1).and_then(Bucket::value),
            Some(12.0)
        );
    }

    #[test]
    fn the_sustained_value_needs_three_read_buckets_and_ignores_one_spike() {
        let mut r = BucketRing::default();
        for (i, v) in [5.0, 6.0, 90.0, 7.0, 6.0].into_iter().enumerate() {
            let _ = r.push(T0 + i as i64 * 10_000, Some(v), false);
        }
        let k0 = T0 / BUCKET_MS;
        assert_eq!(r.sustained(k0 + 1), None, "only two buckets before it");
        assert_eq!(r.sustained(k0 + 2), Some(5.0));
        assert_eq!(r.sustained(k0 + 3), Some(6.0), "the spike is not sustained");
        assert_eq!(r.sustained(k0 + 4), Some(6.0));
    }

    fn reading(at_ms: i64, v: f64) -> Reading {
        Reading {
            at_ms,
            v,
            over: false,
        }
    }

    fn means(cells: &[Cell]) -> Vec<Option<f64>> {
        cells.iter().map(|c| c.mean).collect()
    }

    #[test]
    fn slots_hold_each_reading_in_its_second_and_end_at_the_newest() {
        // A reading a second, 300 ms into each: five 1 s slots end with the one holding the
        // newest, each the reading taken in it.
        let rs: Vec<Reading> = (0..8)
            .map(|i| reading(T0 + 300 + i * 1000, i as f64))
            .collect();
        let at = T0 + 7_300;
        assert_eq!(
            means(&slots_of(&rs, at, SlotGrid::SECONDS, 5, 15_000)),
            [3.0, 4.0, 5.0, 6.0, 7.0].map(Some)
        );
        // Over rides with its reading.
        let mut hot = rs.clone();
        hot[6].over = true;
        let cells = slots_of(&hot, at, SlotGrid::SECONDS, 5, 15_000);
        assert_eq!(
            cells.iter().map(|c| c.over).collect::<Vec<_>>(),
            [false, false, false, true, false]
        );
        // Two readings in one second merge (the lens's half-second cadence).
        let twice = [reading(T0 + 100, 2.0), reading(T0 + 600, 4.0)];
        let one = slots_of(&twice, T0 + 600, SlotGrid::SECONDS, 1, 15_000);
        assert_eq!((one[0].mean, one[0].max), (Some(3.0), Some(4.0)));
        // A reading later than the snapshot is not drawn; the snapshot's slot then holds the
        // reading of its own second only.
        assert_eq!(
            means(&slots_of(&rs, T0 + 5_300, SlotGrid::SECONDS, 2, 15_000)),
            [Some(4.0), Some(5.0)]
        );
    }

    #[test]
    fn a_grid_stands_its_edges_between_the_readings() {
        let clearance = |grid: SlotGrid, t: i64| {
            let into = (t - grid.origin_ms).rem_euclid(grid.slot_ms);
            into.min(grid.slot_ms - into)
        };
        // A reading a second, drifting 3 ms later each, 990 ms into the second: the wall clock's
        // own seconds put every edge on a reading; the grid clear of them puts each reading in
        // the middle of its slot.
        let drift: Vec<i64> = (0..8).map(|i| T0 + 990 + i * 1_003).collect();
        assert!(
            drift
                .iter()
                .any(|&t| !SlotGrid::SECONDS.clear(t, SLOT_GUARD_MS)),
            "CONTROL: the wall clock's seconds have an edge on a reading"
        );
        let grid = SlotGrid::clear_of(&drift, SLOT_MS);
        assert_eq!(grid.slot_ms, SLOT_MS);
        for &t in &drift {
            assert!(clearance(grid, t) >= 480, "{grid:?} {t}");
        }
        for pair in drift.windows(2) {
            assert_eq!(grid.opened(pair[0], pair[1]), 1, "{grid:?} {pair:?}");
        }
        // The lens's two readings a second: each a quarter of a slot inside an edge, so a slot
        // holds one of each and opens once a second.
        let lens: Vec<i64> = (0..8).map(|i| T0 + 40 + i * 500).collect();
        let grid = SlotGrid::clear_of(&lens, SLOT_MS);
        for &t in &lens {
            assert!(clearance(grid, t) >= 240, "{grid:?} {t}");
        }
        let opened: Vec<i64> = lens.windows(2).map(|p| grid.opened(p[0], p[1])).collect();
        assert_eq!(opened.iter().sum::<i64>(), 3, "{opened:?}");
        assert!(opened.iter().all(|&o| o == 0 || o == 1), "{opened:?}");
        // One reading: the edge half a slot from it; none: the wall clock's seconds.
        let one = SlotGrid::clear_of(&[T0 + 200], SLOT_MS);
        assert_eq!(one.origin_ms, 700);
        assert_eq!(SlotGrid::clear_of(&[], SLOT_MS), SlotGrid::SECONDS);
        // Slots start at the origin: with edges 700 ms into each second, a reading at 650 and
        // one at 750 stand in neighbouring slots.
        assert_eq!(one.opened(T0 + 650, T0 + 750), 1);
        assert_eq!(one.opened(T0 + 750, T0 + 1_650), 0);
        assert!(one.clear(T0 + 200, SLOT_GUARD_MS) && !one.clear(T0 + 690, SLOT_GUARD_MS));
        let rs = [reading(T0 + 650, 1.0), reading(T0 + 750, 2.0)];
        assert_eq!(
            means(&slots_of(&rs, T0 + 750, one, 2, 15_000)),
            [Some(1.0), Some(2.0)]
        );
        assert_eq!(
            means(&slots_of(&rs, T0 + 750, SlotGrid::SECONDS, 2, 15_000)),
            [None, Some(1.5)],
            "CONTROL: on the wall clock's seconds the two merge"
        );
    }

    #[test]
    fn an_empty_slot_takes_the_reading_that_speaks_for_it_or_stays_unread() {
        // Readings 5 s apart (aterm in the background): each speaks for the four seconds before
        // it that hold no reading of their own.
        let rs = [
            reading(T0, 1.0),
            reading(T0 + 5_000, 2.0),
            reading(T0 + 10_000, 3.0),
        ];
        assert_eq!(
            means(&slots_of(&rs, T0 + 10_000, SlotGrid::SECONDS, 11, 15_000)),
            [1.0, 2.0, 2.0, 2.0, 2.0, 2.0, 3.0, 3.0, 3.0, 3.0, 3.0].map(Some)
        );
        // A gap over the cover is unread, never filled.
        let gap = [reading(T0, 1.0), reading(T0 + 20_000, 2.0)];
        let cells = slots_of(&gap, T0 + 20_000, SlotGrid::SECONDS, 3, 15_000);
        assert_eq!(means(&cells), [None, None, Some(2.0)]);
        // Before the first reading nothing speaks; a series not read at the snapshot's moment
        // leaves its last slot unread.
        assert_eq!(
            means(&slots_of(&rs, T0 + 12_000, SlotGrid::SECONDS, 4, 15_000)),
            [Some(3.0), Some(3.0), None, None],
            "nothing after the last reading speaks for the slots after it"
        );
        assert_eq!(
            means(&slots_of(&[], T0, SlotGrid::SECONDS, 2, 15_000)),
            [None, None]
        );
        let early = slots_of(&rs, T0 + 10_000, SlotGrid::SECONDS, 13, 15_000);
        assert_eq!(&means(&early)[..2], [None, None], "{early:?}");
    }

    #[test]
    fn a_ring_keeps_its_readings_with_their_over_marks() {
        let mut r = BucketRing::default();
        let _ = r.push(T0, Some(1.0), false);
        let _ = r.push(T0 + 1_000, None, false);
        let _ = r.push(T0 + 2_000, Some(3.0), true);
        assert_eq!(
            r.readings(),
            [
                Reading {
                    at_ms: T0,
                    v: 1.0,
                    over: false
                },
                Reading {
                    at_ms: T0 + 2_000,
                    v: 3.0,
                    over: true
                },
            ],
            "an unread reading is no reading"
        );
        // The minute it keeps, and no more.
        let _ = r.push(T0 + 2_000 + RECENT_KEEP_MS, Some(4.0), false);
        assert_eq!(r.readings().len(), 1);
    }

    #[test]
    fn multi_scale_presets_grow_older_boxes_wider_and_place_ticks_on_exact_boundaries() {
        for spec in [
            MultiScaleSpec::FULL_HOUR,
            MultiScaleSpec::FOLD_15M,
            MultiScaleSpec::COMPACT_5M,
            MultiScaleSpec::MINI_1M,
        ] {
            let slots = spec.slots();
            assert_eq!(slots.len(), spec.slot_count());
            assert!(slots.last().is_some_and(|s| s.is_live_now));
            assert!(slots[..slots.len() - 1].iter().all(|s| !s.is_live_now));
            assert!((slots[0].x0 - 0.0).abs() < 1e-6);
            assert!((slots.last().unwrap().x1 - 1.0).abs() < 1e-6);
            for pair in slots[..slots.len() - 1].windows(2) {
                assert!(
                    pair[0].weight >= pair[1].weight,
                    "historical boxes grow wider toward older left bands: {pair:?}"
                );
                assert_eq!(pair[0].age_lo_ms, pair[1].age_hi_ms);
            }
            let ticks = spec.axis_ticks();
            assert_eq!(ticks.len(), spec.ticks.len());
            for pair in ticks.windows(2) {
                assert!(
                    pair[0].x_frac < pair[1].x_frac,
                    "ticks increase left to right"
                );
            }
        }
        assert_eq!(MultiScaleSpec::FULL_HOUR.horizon_ms(), 3_600_000);
        assert_eq!(MultiScaleSpec::FOLD_15M.horizon_ms(), 900_000);
        assert_eq!(MultiScaleSpec::COMPACT_5M.horizon_ms(), 300_000);
        assert_eq!(MultiScaleSpec::MINI_1M.horizon_ms(), 60_000);
    }

    #[test]
    fn multi_scale_marks_sleep_and_crash_gaps_without_smearing_readings() {
        let mut ring = BucketRing::default();
        let now = T0 + 120_000;
        let _ = ring.push(now - 90_000, Some(20.0), false);
        let _ = ring.push(now - 10_000, Some(50.0), false);
        let _ = ring.push(now - 1_000, Some(80.0), false);
        let gaps = [
            TimeGap {
                start_ms: now - 50_000,
                end_ms: now - 20_000,
                kind: GapKind::Sleep,
            },
            TimeGap {
                start_ms: now - 8_000,
                end_ms: now - 4_000,
                kind: GapKind::Crash,
            },
        ];
        let scaled = multi_scale_cells(
            &ring.cells(),
            ring.open_k().unwrap_or(0),
            &ring.readings(),
            now,
            MultiScaleSpec::COMPACT_5M,
            15_000,
            &gaps,
        );
        assert_eq!(scaled.last().unwrap().cell.mean, Some(80.0));
        assert!(
            scaled
                .iter()
                .any(|s| s.gap == Some(GapKind::Sleep) && s.cell.mean.is_none())
        );
        assert!(
            scaled
                .iter()
                .any(|s| s.gap == Some(GapKind::Crash) && s.cell.mean.is_none())
        );
        let x_crash = MultiScaleSpec::COMPACT_5M
            .x_frac_of_age_ms(6_000)
            .expect("6s inside 5m horizon");
        let x_10s = MultiScaleSpec::COMPACT_5M
            .x_frac_of_age_ms(10_000)
            .expect("10s boundary");
        assert!(x_crash > x_10s && x_crash < 1.0);
    }
}
