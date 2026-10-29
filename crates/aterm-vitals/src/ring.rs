// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wall-clock bucketed rings: an hour of ten-second buckets and six hours of ten-minute ones.

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
/// Milliseconds one long bucket covers: ten minutes, what a ring keeps past its hour.
pub const LONG_BUCKET_MS: i64 = 600_000;
/// Long buckets a ring keeps: six hours and the open one, what the deck's 1 h boxes read.
pub const LONG_BUCKETS: usize = 37;
/// How far back a ring's ten-second buckets reach, ms; a strip box older reads the long ones.
pub const RING_REACH_MS: i64 = RING_BUCKETS as i64 * BUCKET_MS;
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

    /// Merge cells each spanning `w` ms of one box: the read ones' mean weighted by `w`, the
    /// largest max, over when any is; a cell spanning no time counts for nothing.
    #[must_use]
    pub fn merge_weighted(cells: impl IntoIterator<Item = (Cell, i64)>) -> Cell {
        let (mut sum, mut weight) = (0.0, 0.0);
        let mut out = Cell::default();
        for (c, w) in cells {
            if w <= 0 {
                continue;
            }
            if let Some(m) = c.mean {
                sum += m * w as f64;
                weight += w as f64;
            }
            if let Some(x) = c.max {
                out.max = Some(out.max.map_or(x, |o| o.max(x)));
            }
            out.over |= c.over;
        }
        out.mean = (weight > 0.0).then(|| sum / weight);
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
/// and a strip's finest boxes ([`multi_scale_cells`]).
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

/// Six hours of ten-minute buckets, keyed by the wall clock (`at_unix_ms / LONG_BUCKET_MS`).
#[derive(Clone, Debug, Default, PartialEq)]
struct Long {
    /// The key of `b[0]`.
    first_k: i64,
    b: VecDeque<Bucket>,
}

impl Long {
    fn open_k(&self) -> Option<i64> {
        (!self.b.is_empty()).then(|| self.first_k + self.b.len() as i64 - 1)
    }

    /// Record a reading taken at `at_unix_ms`: a step back of one bucket lands in the open one,
    /// a longer one re-keys the buckets held so the reading opens the next, as the ring does.
    fn push(&mut self, at_unix_ms: i64, v: Option<f64>, over: bool) {
        let k = at_unix_ms.div_euclid(LONG_BUCKET_MS);
        let cap = LONG_BUCKETS as i64;
        match self.open_k() {
            None => {
                self.first_k = k;
                self.b.push_back(Bucket::default());
            }
            Some(open) if k > open => {
                if k - open >= cap {
                    self.b.clear();
                    self.first_k = k;
                } else {
                    for _ in 1..k - open {
                        self.b.push_back(Bucket::default());
                    }
                }
                self.b.push_back(Bucket::default());
            }
            Some(open) if open - k <= 1 => {}
            Some(open) => {
                self.first_k -= open + 1 - k;
                self.b.push_back(Bucket::default());
            }
        }
        while self.b.len() > LONG_BUCKETS {
            self.b.pop_front();
            self.first_k += 1;
        }
        if let Some(v) = v
            && let Some(b) = self.b.back_mut()
        {
            b.add(v, over);
        }
    }
}

/// An hour of 10 s buckets and six hours of ten-minute ones, keyed by the wall clock.
#[derive(Clone, Debug, PartialEq)]
pub struct BucketRing {
    cap: usize,
    /// The key of `b[0]`.
    first_k: i64,
    b: VecDeque<Bucket>,
    /// The readings behind "now".
    recent: Recent,
    /// The ten-minute buckets the deck's 1 h boxes read.
    long: Long,
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
            long: Long::default(),
        }
    }

    /// Record a reading taken at `at_unix_ms`.
    pub fn push(&mut self, at_unix_ms: i64, v: Option<f64>, over: bool) -> Option<i64> {
        let v = v.filter(|x| x.is_finite());
        self.long.push(at_unix_ms, v, over);
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

    /// Every ten-minute bucket as a cell, oldest first; the last is the open one ([`Self::long_k`]).
    #[must_use]
    pub fn long_cells(&self) -> Vec<Cell> {
        self.long.b.iter().map(Bucket::cell).collect()
    }

    /// The open ten-minute bucket's key (Unix ms `[600_000 * k, 600_000 * (k + 1))`); `None`
    /// before the first push.
    #[inline]
    #[must_use]
    pub fn long_k(&self) -> Option<i64> {
        self.long.open_k()
    }

    /// The last minute of readings, oldest first ([`Tiers::recent`]).
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

    /// Compact snapshot of non-empty 10 s and ten-minute buckets and last-minute readings for
    /// persistence.
    #[must_use]
    pub fn saved(&self) -> SavedRing {
        let buckets = saved_buckets(self.first_k, &self.b);
        let long = saved_buckets(self.long.first_k, &self.long.b);
        let recent = self
            .recent
            .readings()
            .into_iter()
            .filter(|r| r.v.is_finite())
            .map(|r| (r.at_ms, r.v, r.over))
            .collect();
        SavedRing {
            buckets,
            recent,
            long,
        }
    }

    /// Restore a ring persisted in an earlier run up to `now_unix_ms`, pruning stale or future
    /// entries outside the 1-hour window (six hours for the ten-minute buckets).
    #[must_use]
    pub fn restored(saved: &SavedRing, now_unix_ms: i64) -> Self {
        let mut ring = Self::default();
        if now_unix_ms <= 0 {
            return ring;
        }
        (ring.first_k, ring.b) = restored_buckets(
            &saved.buckets,
            now_unix_ms.div_euclid(BUCKET_MS),
            RING_BUCKETS,
        );
        (ring.long.first_k, ring.long.b) = restored_buckets(
            &saved.long,
            now_unix_ms.div_euclid(LONG_BUCKET_MS),
            LONG_BUCKETS,
        );
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

/// `b`'s read buckets, the first keyed `first_k`, as [`SavedBucket`]s.
fn saved_buckets(first_k: i64, b: &VecDeque<Bucket>) -> Vec<SavedBucket> {
    b.iter()
        .enumerate()
        .filter_map(|(i, b)| {
            (b.n > 0 && b.sum.is_finite() && b.max.is_finite()).then_some((
                first_k + i as i64,
                b.sum,
                b.n,
                b.max,
                b.over_n,
            ))
        })
        .collect()
}

/// The buckets of `saved` keyed `now_k - (cap - 1)..=now_k`, as a ring's `(first_k, b)`: the
/// newest last, the unread ones between them empty, at most `cap`.
fn restored_buckets(saved: &[SavedBucket], now_k: i64, cap: usize) -> (i64, VecDeque<Bucket>) {
    let min_k = now_k - (cap as i64 - 1);
    let mut valid: Vec<SavedBucket> = saved
        .iter()
        .copied()
        .filter(|&(k, sum, n, max, _)| {
            n > 0 && sum.is_finite() && max.is_finite() && k >= min_k && k <= now_k
        })
        .collect();
    valid.sort_by_key(|&(k, ..)| k);
    valid.dedup_by_key(|&mut (k, ..)| k);
    let (Some(&(first, ..)), Some(&(last_k, ..))) = (valid.first(), valid.last()) else {
        return (0, VecDeque::new());
    };
    let len = usize::try_from(last_k - first + 1).unwrap_or(0).min(cap);
    let first_k = last_k - len as i64 + 1;
    let mut b = VecDeque::from(vec![Bucket::default(); len]);
    for (k, sum, n, max, over_n) in valid {
        if let Ok(idx) = usize::try_from(k - first_k)
            && let Some(slot) = b.get_mut(idx)
        {
            *slot = Bucket {
                sum,
                n,
                max,
                over_n: over_n.min(n),
            };
        }
    }
    (first_k, b)
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
    /// Non-empty ten-minute buckets within six hours, oldest first, keyed by
    /// [`LONG_BUCKET_MS`].
    pub long: Vec<SavedBucket>,
}

impl SavedRing {
    /// True when neither buckets nor recent readings are held.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty() && self.recent.is_empty() && self.long.is_empty()
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

/// One resolution band in a [`MultiScaleSpec`], ordered newest (left) to oldest (right).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScaleBand {
    /// Number of consecutive boxes in this band.
    pub slots: u16,
    /// Wall-clock duration each box in this band covers, in milliseconds.
    pub period_ms: i64,
    /// Relative horizontal width weight of each box in this band.
    pub weight: u16,
    /// True for the far-left instantaneous live `now` box.
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
    /// True when this slot is the far-left real-time `now` box.
    pub is_live_now: bool,
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

/// Multi-scale progressive time-series specification: the live `now` box at the left, then fine
/// real-time boxes growing into wider aggregated buckets toward the right, so the strip reads
/// outward from now (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MultiScaleSpec {
    /// Resolution bands ordered newest (left, the live box first) to oldest (right).
    pub bands: &'static [ScaleBand],
    /// Horizon ticks `(age_ms, label)` ordered left to right, `now` first.
    pub ticks: &'static [(i64, &'static str)],
}

const BANDS_FULL_6H: [ScaleBand; 6] = [
    ScaleBand {
        slots: 1,
        period_ms: 1_000,
        weight: 10,
        live_now: true,
    },
    ScaleBand {
        slots: 9,
        period_ms: 1_000,
        weight: 4,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 10_000,
        weight: 5,
        live_now: false,
    },
    ScaleBand {
        slots: 9,
        period_ms: 60_000,
        weight: 5,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 600_000,
        weight: 7,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 3_600_000,
        weight: 10,
        live_now: false,
    },
];

const TICKS_FULL_6H: [(i64, &str); 6] = [
    (0, "now"),
    (10_000, "\u{2013}10s"),
    (60_000, "\u{2013}1m"),
    (600_000, "\u{2013}10m"),
    (3_600_000, "\u{2013}1h"),
    (21_600_000, "\u{2013}6h"),
];

const BANDS_FOLD_4H: [ScaleBand; 6] = [
    ScaleBand {
        slots: 1,
        period_ms: 1_000,
        weight: 2,
        live_now: true,
    },
    ScaleBand {
        slots: 9,
        period_ms: 1_000,
        weight: 1,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 10_000,
        weight: 1,
        live_now: false,
    },
    ScaleBand {
        slots: 9,
        period_ms: 60_000,
        weight: 1,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 600_000,
        weight: 2,
        live_now: false,
    },
    ScaleBand {
        slots: 3,
        period_ms: 3_600_000,
        weight: 2,
        live_now: false,
    },
];

const TICKS_FOLD_4H: [(i64, &str); 6] = [
    (0, "now"),
    (10_000, "\u{2013}10s"),
    (60_000, "\u{2013}1m"),
    (600_000, "\u{2013}10m"),
    (3_600_000, "\u{2013}1h"),
    (14_400_000, "\u{2013}4h"),
];

const BANDS_COMPACT_6M: [ScaleBand; 4] = [
    ScaleBand {
        slots: 1,
        period_ms: 1_000,
        weight: 2,
        live_now: true,
    },
    ScaleBand {
        slots: 9,
        period_ms: 1_000,
        weight: 1,
        live_now: false,
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
        weight: 2,
        live_now: false,
    },
];

const TICKS_COMPACT_6M: [(i64, &str); 4] = [
    (0, "now"),
    (10_000, "\u{2013}10s"),
    (60_000, "\u{2013}1m"),
    (360_000, "\u{2013}6m"),
];

const BANDS_MINI_1M: [ScaleBand; 3] = [
    ScaleBand {
        slots: 1,
        period_ms: 1_000,
        weight: 1,
        live_now: true,
    },
    ScaleBand {
        slots: 9,
        period_ms: 1_000,
        weight: 1,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 10_000,
        weight: 2,
        live_now: false,
    },
];

const TICKS_MINI_1M: [(i64, &str); 3] =
    [(0, "now"), (10_000, "\u{2013}10s"), (60_000, "\u{2013}1m")];

impl MultiScaleSpec {
    /// Full's six hours: the live second, nine more 1 s boxes, five of 10 s, nine of 1 min,
    /// five of 10 min and five of 1 h, each size starting where its age is its own block's
    /// (`docs/DESIGN-stats-deck-owner-2026-10-26.md` §4), 34 slots.
    pub const FULL_6H: Self = Self {
        bands: &BANDS_FULL_6H,
        ticks: &TICKS_FULL_6H,
    };

    /// Four hours: [`Self::FULL_6H`]'s blocks with three of 1 h, 32 slots. The 1 s, 10 s and
    /// 1 min blocks are one width and the live, 10 min and 1 h ones twice it, so on Folded's
    /// 129 pt strip a 1 s block is 3 pt or more and "–1m" and "–10m" stand clear of "4h ago".
    pub const FOLD_4H: Self = Self {
        bands: &BANDS_FOLD_4H,
        ticks: &TICKS_FOLD_4H,
    };

    /// Six minutes: the 1 s, 10 s and five 1 min blocks; too narrow for the 10 min and 1 h
    /// ones, 20 slots. The live and 1 min blocks are twice the 1 s and 10 s ones, so on a 79 pt
    /// strip a 1 s block is 3 pt or more and "–10s" stands clear of "6m ago".
    pub const COMPACT_6M: Self = Self {
        bands: &BANDS_COMPACT_6M,
        ticks: &TICKS_COMPACT_6M,
    };

    /// One minute: the 1 s and 10 s blocks alone, 15 slots, the 10 s ones twice as wide, so on a
    /// 59 pt strip a 1 s block is 2.95 pt (3 device px at 1×) and "–10s", at the strip's
    /// middle, stands clear of "–1m".
    pub const MINI_1M: Self = Self {
        bands: &BANDS_MINI_1M,
        ticks: &TICKS_MINI_1M,
    };

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

    /// Compute normalized `[x0, x1]` and `[age_lo_ms, age_hi_ms]` layout for every slot, newest
    /// first: the live `now` box at `x0 = 0`, then each older box to the right of the last.
    #[must_use]
    pub fn slots(&self) -> Vec<ProgSlot> {
        let total_w = self.total_weight() as f32;
        let mut age = 0_i64;
        let mut acc_w = 0u32;
        let mut out = Vec::with_capacity(self.slot_count());
        for band in self.bands {
            for _ in 0..band.slots {
                let next_w = acc_w + u32::from(band.weight);
                let next_age = age + band.period_ms;
                out.push(ProgSlot {
                    x0: acc_w as f32 / total_w,
                    x1: next_w as f32 / total_w,
                    age_hi_ms: next_age,
                    age_lo_ms: age,
                    weight: band.weight,
                    is_live_now: band.live_now,
                });
                acc_w = next_w;
                age = next_age;
            }
        }
        out
    }

    /// Map an event age `age_ms` (`now_unix_ms - event_unix_ms`) to its exact continuous
    /// normalized X coordinate in `0.0..=1.0`, growing with age (now is `0`, the horizon `1`), or
    /// `None` when older than [`Self::horizon_ms`].
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
                let into = (age - slot.age_lo_ms) as f32 / span;
                return Some((slot.x0 + (slot.x1 - slot.x0) * into).clamp(0.0, 1.0));
            }
        }
        Some(1.0)
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

/// What a strip resamples ([`multi_scale_cells`]): a ring's hour of 10 s cells, its six hours
/// of ten-minute cells, and its last minute of readings.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tiers<'a> {
    /// The 10 s cells, oldest first, the last keyed `ring_k` ([`BucketRing::cells`]).
    pub ring: &'a [Cell],
    /// The last 10 s cell's key ([`BucketRing::open_k`]).
    pub ring_k: i64,
    /// The ten-minute cells, oldest first, the last keyed `long_k` ([`BucketRing::long_cells`]).
    pub long: &'a [Cell],
    /// The last ten-minute cell's key ([`BucketRing::long_k`]).
    pub long_k: i64,
    /// The last minute of readings, oldest first ([`BucketRing::readings`]).
    pub recent: &'a [Reading],
}

/// Resample `tiers` into progressive [`ScaleCell`]s for `spec` at `now_unix_ms`, marking
/// discontinuous `gaps` without smearing adjacent readings across them: a box within the last
/// minute reads the readings, one within [`RING_REACH_MS`] the 10 s cells, and an older one the
/// ten-minute cells, each weighted by how much of the box it spans.
#[must_use]
pub fn multi_scale_cells(
    tiers: Tiers<'_>,
    now_unix_ms: i64,
    spec: MultiScaleSpec,
    cover_ms: i64,
    gaps: &[TimeGap],
) -> Vec<ScaleCell> {
    let slots = spec.slots();
    let now_k = now_unix_ms.div_euclid(BUCKET_MS);
    let (ring_cells, last_k, recent) = (tiers.ring, tiers.ring_k, tiers.recent);
    let first_k = last_k + 1 - ring_cells.len() as i64;
    let ring_at = |k: i64| -> Option<Cell> {
        (k >= first_k && k <= last_k && k <= now_k)
            .then(|| usize::try_from(k - first_k).ok())
            .flatten()
            .and_then(|i| ring_cells.get(i).copied())
    };
    let long_first = tiers.long_k + 1 - tiers.long.len() as i64;
    let long_cell = |lo_ms: i64, hi_ms: i64| -> Cell {
        let (k_lo, k_hi) = (
            lo_ms.div_euclid(LONG_BUCKET_MS),
            hi_ms
                .saturating_sub(1)
                .max(lo_ms)
                .div_euclid(LONG_BUCKET_MS),
        );
        Cell::merge_weighted(
            (k_lo.max(long_first)..=k_hi.min(tiers.long_k)).filter_map(|k| {
                let cell = tiers
                    .long
                    .get(usize::try_from(k - long_first).ok()?)
                    .copied()?;
                let (b_lo, b_hi) = (k * LONG_BUCKET_MS, (k + 1) * LONG_BUCKET_MS);
                Some((cell, hi_ms.min(b_hi) - lo_ms.max(b_lo)))
            }),
        )
    };
    let recent_valid = &recent[..recent.partition_point(|r| r.at_ms <= now_unix_ms)];
    slots
        .into_iter()
        .map(|slot| {
            let lo_ms = now_unix_ms.saturating_sub(slot.age_hi_ms);
            let hi_ms = now_unix_ms.saturating_sub(slot.age_lo_ms);
            let any_gap = gaps.iter().any(|g| g.overlap_ms(lo_ms, hi_ms) > 0);
            // The 10 s cells the box spans, through bucket `k_max` at the newest.
            let ring_cell = |k_max: i64| {
                let k_lo = lo_ms.div_euclid(BUCKET_MS);
                let k_hi = hi_ms.saturating_sub(1).max(lo_ms).div_euclid(BUCKET_MS);
                Cell::merge((k_lo..=k_hi.min(k_max)).filter_map(ring_at))
            };
            let mut raw = if slot.age_lo_ms >= RING_REACH_MS {
                long_cell(lo_ms, hi_ms)
            } else if slot.age_hi_ms <= RECENT_KEEP_MS && !recent_valid.is_empty() {
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
                    // Every reading kept was taken at or after the box's newer edge, so the
                    // bucket holding the oldest of them is taken as read after the box (with a
                    // restored bucket's readings the minute did not keep): only the buckets
                    // before it speak for the box.
                    let first_read_k = recent_valid
                        .first()
                        .map_or(i64::MAX, |r| r.at_ms.div_euclid(BUCKET_MS));
                    ring_cell(first_read_k.saturating_sub(1))
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
                ring_cell(i64::MAX)
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
    fn multi_scale_presets_read_outward_from_now_with_older_boxes_wider() {
        for spec in [
            MultiScaleSpec::FULL_6H,
            MultiScaleSpec::FOLD_4H,
            MultiScaleSpec::COMPACT_6M,
            MultiScaleSpec::MINI_1M,
        ] {
            let slots = spec.slots();
            assert_eq!(slots.len(), spec.slot_count());
            // Newest first: the live box at the left end, holding now.
            let live = slots[0];
            assert!(live.is_live_now && live.x0.abs() < 1e-6 && live.age_lo_ms == 0);
            assert!(slots[1..].iter().all(|s| !s.is_live_now));
            assert!((slots.last().unwrap().x1 - 1.0).abs() < 1e-6);
            assert_eq!(slots.last().unwrap().age_hi_ms, spec.horizon_ms());
            for pair in slots.windows(2) {
                assert_eq!(pair[0].x1, pair[1].x0, "the boxes tile the strip");
                assert_eq!(pair[0].age_hi_ms, pair[1].age_lo_ms, "and the age");
            }
            for pair in slots[1..].windows(2) {
                assert!(
                    pair[0].weight <= pair[1].weight,
                    "historical boxes grow wider toward older right bands: {pair:?}"
                );
            }
            // x grows with age: now at 0, the horizon at 1, every box edge on its own x.
            assert_eq!(spec.x_frac_of_age_ms(0), Some(0.0));
            assert_eq!(spec.x_frac_of_age_ms(spec.horizon_ms()), Some(1.0));
            assert_eq!(spec.x_frac_of_age_ms(spec.horizon_ms() + 1), None);
            for slot in &slots {
                let at = |age| spec.x_frac_of_age_ms(age).expect("inside the horizon");
                assert!((at(slot.age_lo_ms) - slot.x0).abs() < 1e-6, "{slot:?}");
                assert!((at(slot.age_hi_ms) - slot.x1).abs() < 1e-6, "{slot:?}");
            }
            let xs: Vec<f32> = (0..=spec.horizon_ms())
                .step_by(500)
                .filter_map(|age| spec.x_frac_of_age_ms(age))
                .collect();
            assert!(xs.windows(2).all(|w| w[0] <= w[1]), "x grows with age");
            assert!(
                spec.ticks.windows(2).all(|w| w[0].0 < w[1].0),
                "ticks read outward from now: tick_ages takes them coarsest first by reversing \
                 this order"
            );
        }
        assert_eq!(MultiScaleSpec::FULL_6H.horizon_ms(), 6 * 3_600_000);
        assert_eq!(MultiScaleSpec::FOLD_4H.horizon_ms(), 4 * 3_600_000);
        assert_eq!(MultiScaleSpec::COMPACT_6M.horizon_ms(), 360_000);
        assert_eq!(MultiScaleSpec::MINI_1M.horizon_ms(), 60_000);
    }

    /// The owner's blocks (2026-10-26): 1 s, 10 s, 1 min, 10 min and 1 h, at least three of each
    /// size a strip draws and five of each in Full, each size starting at the age of its own
    /// block, where a tick names it; a narrower preset drops the largest sizes, never a middle
    /// one.
    #[test]
    fn every_block_size_starts_at_its_own_age_with_three_to_five_of_it() {
        const SIZES: [i64; 5] = [1_000, 10_000, 60_000, 600_000, 3_600_000];
        for (spec, sizes, least) in [
            (MultiScaleSpec::FULL_6H, 5, 5),
            (MultiScaleSpec::FOLD_4H, 5, 3),
            (MultiScaleSpec::COMPACT_6M, 3, 5),
            (MultiScaleSpec::MINI_1M, 2, 5),
        ] {
            let slots = spec.slots();
            let mut age = 0;
            for &size in &SIZES[..sizes] {
                let of: Vec<&ProgSlot> = slots
                    .iter()
                    .filter(|s| s.age_hi_ms - s.age_lo_ms == size)
                    .collect();
                assert!(of.len() >= least, "{size} ms: {} boxes", of.len());
                assert_eq!(of[0].age_lo_ms, age, "{size} ms blocks follow the smaller");
                assert!(
                    of.windows(2).all(|w| w[0].age_hi_ms == w[1].age_lo_ms),
                    "{size} ms blocks stand together"
                );
                if size > 1_000 {
                    assert_eq!(age, size, "a size starts where its age is its own block");
                    assert!(
                        spec.ticks.iter().any(|&(at, _)| at == size),
                        "a tick names where {size} ms blocks start"
                    );
                }
                age = of.last().map_or(age, |s| s.age_hi_ms);
            }
            assert_eq!(age, spec.horizon_ms(), "no other size is drawn");
            assert_eq!(spec.ticks.last().map(|t| t.0), Some(spec.horizon_ms()));
        }
    }

    #[test]
    fn a_ring_keeps_six_hours_of_ten_minute_buckets_and_restores_them() {
        let mut r = BucketRing::default();
        assert_eq!((r.long_k(), r.long_cells().len()), (None, 0));
        let t0 = T0 - T0.rem_euclid(LONG_BUCKET_MS);
        for i in 0..8 * 360 {
            let at = t0 + i * 10_000;
            let _ = r.push(at, Some(if at < t0 + 3_600_000 { 1.0 } else { 3.0 }), false);
        }
        let k_now = (t0 + (8 * 360 - 1) * 10_000).div_euclid(LONG_BUCKET_MS);
        assert_eq!(r.long_k(), Some(k_now));
        let cells = r.long_cells();
        assert_eq!(cells.len(), LONG_BUCKETS, "six hours and the open bucket");
        assert!(
            cells.iter().all(|c| c.mean == Some(3.0)),
            "the first hour aged out"
        );
        assert_eq!(r.len(), RING_BUCKETS, "the 10 s ring still keeps its hour");

        let saved = r.saved();
        assert_eq!(saved.long.len(), LONG_BUCKETS);
        let now = t0 + 8 * 3_600_000 - 1;
        let back = BucketRing::restored(&saved, now);
        assert_eq!(back.long_cells(), r.long_cells());
        assert_eq!(back.long_k(), r.long_k());
        let later = BucketRing::restored(&saved, now + 3 * 3_600_000);
        assert_eq!(
            later.long_cells().len(),
            LONG_BUCKETS - 18,
            "three hours on, the oldest three are gone"
        );
        let old = SavedRing {
            long: Vec::new(),
            ..saved
        };
        assert!(
            BucketRing::restored(&old, now).long_cells().is_empty(),
            "a file written before the long buckets restores none"
        );

        let mut back_step = BucketRing::default();
        let _ = back_step.push(t0 + 2 * LONG_BUCKET_MS, Some(1.0), false);
        let _ = back_step.push(t0 + LONG_BUCKET_MS + 5_000, Some(3.0), false);
        assert_eq!(
            back_step
                .long_cells()
                .iter()
                .map(|c| c.mean)
                .collect::<Vec<_>>(),
            [Some(2.0)],
            "a step back of one bucket lands in the open one"
        );
        let _ = back_step.push(t0 - 3 * 3_600_000, Some(5.0), false);
        assert_eq!(
            back_step.long_k(),
            Some((t0 - 3 * 3_600_000).div_euclid(LONG_BUCKET_MS))
        );
        assert_eq!(
            back_step.long_cells().last().and_then(|c| c.mean),
            Some(5.0)
        );
    }

    #[test]
    fn hour_boxes_read_the_ten_minute_buckets_by_how_much_of_each_they_span() {
        let mut r = BucketRing::default();
        let t0 = T0 - T0.rem_euclid(LONG_BUCKET_MS);
        for i in 0..6 * 360 {
            let at = t0 + i * 10_000;
            let v = (at.div_euclid(LONG_BUCKET_MS) - t0 / LONG_BUCKET_MS) as f64;
            let _ = r.push(at, Some(v), false);
        }
        let now = t0 + 6 * 3_600_000 - 1 + 300_000;
        let (ring, long, recent) = (r.cells(), r.long_cells(), r.readings());
        let tiers = Tiers {
            ring: &ring,
            ring_k: r.open_k().unwrap_or(0),
            long: &long,
            long_k: r.long_k().unwrap_or(0),
            recent: &recent,
        };
        let cells = multi_scale_cells(tiers, now, MultiScaleSpec::FULL_6H, 2_500, &[]);
        let hours: Vec<&ScaleCell> = cells
            .iter()
            .filter(|c| c.slot.age_hi_ms - c.slot.age_lo_ms == 3_600_000)
            .collect();
        assert_eq!(hours.len(), 5);
        // Bucket k of the six hours read k - first, so an hour box reads the mean of its
        // buckets' keys weighted by how many of its ms each holds.
        let first = r.long_k().unwrap_or(0) + 1 - long.len() as i64;
        assert_eq!(first, t0 / LONG_BUCKET_MS);
        for h in &hours {
            let (lo, hi) = (now - h.slot.age_hi_ms, now - h.slot.age_lo_ms);
            let (mut sum, mut ms) = (0.0, 0.0);
            for k in lo.div_euclid(LONG_BUCKET_MS)..=(hi - 1).div_euclid(LONG_BUCKET_MS) {
                let ov = hi.min((k + 1) * LONG_BUCKET_MS) - lo.max(k * LONG_BUCKET_MS);
                sum += (k - first) as f64 * ov as f64;
                ms += ov as f64;
            }
            let got = h.cell.mean.expect("an hour the ring read");
            assert!(
                (got - sum / ms).abs() < 1e-6,
                "{got} against {}: {h:?}",
                sum / ms
            );
        }
        assert!(
            hours.windows(2).all(|w| w[0].cell.mean > w[1].cell.mean),
            "older hours read the older, lower buckets"
        );
        assert!(
            cells
                .iter()
                .filter(|c| c.slot.age_lo_ms < RING_REACH_MS && c.slot.age_lo_ms >= 600_000)
                .all(|c| c.cell.mean.is_some()),
            "the 10 min boxes read the ring"
        );
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
        let (cells, recent) = (ring.cells(), ring.readings());
        let tiers = Tiers {
            ring: &cells,
            ring_k: ring.open_k().unwrap_or(0),
            recent: &recent,
            ..Tiers::default()
        };
        let scaled = multi_scale_cells(tiers, now, MultiScaleSpec::COMPACT_6M, 15_000, &gaps);
        assert!(scaled[0].slot.is_live_now, "newest first");
        assert_eq!(scaled[0].cell.mean, Some(80.0));
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
        let x_crash = MultiScaleSpec::COMPACT_6M
            .x_frac_of_age_ms(6_000)
            .expect("6s inside 5m horizon");
        let x_10s = MultiScaleSpec::COMPACT_6M
            .x_frac_of_age_ms(10_000)
            .expect("10s boundary");
        assert!(
            x_crash > 0.0 && x_crash < x_10s,
            "now at the left, x grows with age"
        );
    }

    /// A 10 s box older than a run's first reading draws nothing from that reading's bucket,
    /// whose readings were all taken after the box ended; the buckets before it, an earlier
    /// run's restored ones, still speak for it.
    #[test]
    fn a_10s_box_before_the_first_reading_draws_nothing_read_after_it() {
        let now = T0 + 15_000;
        let box_at = |ring: &BucketRing, age_lo_ms: i64, age_hi_ms: i64| -> Option<f64> {
            let (cells, long, recent) = (ring.cells(), ring.long_cells(), ring.readings());
            let tiers = Tiers {
                ring: &cells,
                ring_k: ring.open_k().unwrap_or(0),
                long: &long,
                long_k: ring.long_k().unwrap_or(0),
                recent: &recent,
            };
            let scaled = multi_scale_cells(tiers, now, MultiScaleSpec::FULL_6H, 15_000, &[]);
            let s = scaled
                .iter()
                .find(|s| s.slot.age_lo_ms == age_lo_ms && s.slot.age_hi_ms == age_hi_ms)
                .expect("the spec has the box");
            s.cell.mean
        };
        // A run whose first reading is mid-bucket, five seconds into T0's 10 s, then one a second.
        let read_on = |ring: &mut BucketRing| {
            for t in (5_000..=15_000).step_by(1_000) {
                let _ = ring.push(T0 + t, Some(1.0), false);
            }
        };
        let mut fresh = BucketRing::default();
        read_on(&mut fresh);
        assert_eq!(
            box_at(&fresh, 9_000, 10_000),
            Some(1.0),
            "the first reading"
        );
        assert_eq!(
            box_at(&fresh, 10_000, 20_000),
            None,
            "the 10 s before the first reading: nothing was read"
        );

        // An earlier run read until 20 s before T0 and is restored with its buckets alone (a
        // strip with no per-second readings, `SavedRing::recent` empty), then the same run reads.
        let mut earlier = BucketRing::default();
        for t in (-100_000_i64..=-20_000).step_by(1_000) {
            let _ = earlier.push(T0 + t, Some(2.0), false);
        }
        let mut saved = earlier.saved();
        saved.recent.clear();
        let mut restored = BucketRing::restored(&saved, T0 + 4_000);
        read_on(&mut restored);
        assert_eq!(
            box_at(&restored, 10_000, 20_000),
            None,
            "between the earlier run's last bucket and the first reading: nothing was read"
        );
        for (lo, hi) in [
            (30_000, 40_000),
            (40_000, 50_000),
            (50_000, 60_000),
            (60_000, 120_000),
        ] {
            assert_eq!(
                box_at(&restored, lo, hi),
                Some(2.0),
                "the earlier run's buckets at {lo}..{hi} ms"
            );
        }
    }
}
