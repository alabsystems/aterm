// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wall-clock bucketed rings: an hour of ten-second buckets and seven hours of ten-minute ones,
//! each bucket an [`Agg`] whose readings weigh by the time they stand for
//! (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §5.2, as `docs/DESIGN-stats-deck-units-2026-11-04.md`
//! §9's F4′ amends it).

use std::collections::VecDeque;

/// Seconds one bucket covers.
pub const BUCKET_S: i64 = 10;
/// Milliseconds one bucket covers.
pub const BUCKET_MS: i64 = BUCKET_S * 1000;
/// Buckets a ring keeps: an hour, what the lens shows.
pub const RING_BUCKETS: usize = 360;
/// The window "now" averages over, ms.
pub const NOW_WINDOW_MS: i64 = 10_000;
/// The longest step back behind a history's newest reading that still lands in its newest
/// bucket, seconds ([`lands`]).
pub const CLAMP_BACK_S: i64 = 60;
/// Milliseconds one long bucket covers: ten minutes, what a ring keeps past its hour.
pub const LONG_BUCKET_MS: i64 = 600_000;
/// Long buckets a ring keeps: seven hours and the open one. The deck's 1 h boxes reach six
/// hours back, and the hour box leaving that horizon still needs its ten-minute children to seven
/// (record §5.2).
pub const LONG_BUCKETS: usize = 43;
/// How far back a ring's ten-second buckets reach, ms; a strip box older reads the long ones.
pub const RING_REACH_MS: i64 = RING_BUCKETS as i64 * BUCKET_MS;
/// How long [`Recent`] keeps a reading, ms: the longest window asked of it.
pub const RECENT_KEEP_MS: i64 = 60_000;
/// The most readings [`Recent`] holds, whatever their cadence.
pub const RECENT_CAP: usize = 256;

/// When a reading was taken and the cadence it was asked at. It stands for the slot of that
/// length centred on it: a reading on the half-slot (at x.500 of a 1 s cadence, 2.5 s into a 5 s
/// one) stands for its own epoch slot exactly, which lies inside one ten-second bucket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Taken {
    /// When it was taken, unix ms.
    pub at_ms: i64,
    /// The cadence it was asked at, ms; 0 for a reading asked at none or taken between the
    /// deck's own (a disk look, a control worker's pull), which stands for 1 ms.
    pub period_ms: u32,
}

impl Taken {
    /// A reading at `at_ms` asked at a cadence of `period_ms`.
    #[inline]
    #[must_use]
    pub const fn at(at_ms: i64, period_ms: u32) -> Self {
        Self { at_ms, period_ms }
    }

    /// The slot `[lo, hi)` it stands for, unix ms: `period_ms` long (at least 1 ms), centred on
    /// `at_ms`.
    #[inline]
    #[must_use]
    pub const fn slot(self) -> (i64, i64) {
        let len = if self.period_ms == 0 {
            1
        } else {
            self.period_ms as i64
        };
        let lo = self.at_ms.saturating_sub(len / 2);
        (lo, lo.saturating_add(len))
    }
}

/// How a reading lands in a history of wall-clock buckets: A1 §1.4 item 9's one back-step rule,
/// which [`BucketRing`], its ten-minute buckets, [`crate::history::FreeHistory`] and
/// [`crate::writes::WriteHour`] share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lands {
    /// The history held nothing: the reading opens its first bucket, keyed as given.
    First(i64),
    /// In the newest bucket: its own, or one up to [`CLAMP_BACK_S`] behind the newest reading.
    Newest,
    /// Ahead of the newest bucket by this many buckets: the reading opens the last of them.
    Ahead(i64),
    /// More than [`CLAMP_BACK_S`] behind the newest reading, in an earlier bucket: every held
    /// bucket is re-keyed this many buckets back, onto the epoch grid, so the newest held one is
    /// the one before the reading's and the reading opens a bucket of its own.
    Back(i64),
}

/// How a reading taken at `at_ms` lands in a history of `period_ms` buckets whose newest is
/// keyed `newest_k` and whose newest reading was taken at `newest_ms` ([`Lands`]). A history
/// whose newest reading is not known (restored with its buckets alone) takes the newest bucket's
/// start for it.
#[must_use]
pub fn lands(newest_k: Option<i64>, newest_ms: Option<i64>, at_ms: i64, period_ms: i64) -> Lands {
    let period = period_ms.max(1);
    let k = at_ms.div_euclid(period);
    let Some(open) = newest_k else {
        return Lands::First(k);
    };
    if k > open {
        return Lands::Ahead(k.saturating_sub(open));
    }
    if k == open {
        return Lands::Newest;
    }
    let newest = newest_ms.unwrap_or_else(|| open.saturating_mul(period));
    if i128::from(newest) - i128::from(at_ms) <= i128::from(CLAMP_BACK_S * 1000) {
        Lands::Newest
    } else {
        Lands::Back(open.saturating_sub(k).saturating_add(1))
    }
}

/// One bucket's readings, each weighed by the milliseconds of the bucket it stands for: its read
/// time, its count, the time over the resource's maximum, its time-weighted mean and its least
/// and largest readings (`docs/DESIGN-stats-deck-units-2026-11-04.md` §9, F4′: no m2).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Agg {
    /// The read time, ms: what its readings stand for; 0 is not read.
    pub tau_ms: u32,
    /// How many readings, saturating at `u16::MAX`.
    pub n: u16,
    /// How much of the read time was over the resource's maximum, ms (at most `tau_ms`).
    pub over_ms: u32,
    /// The readings' mean, each weighed by its time (meaningless while `tau_ms` is 0).
    pub mean: f64,
    /// The least reading (meaningless while `n` is 0).
    pub min: f64,
    /// The largest reading (meaningless while `n` is 0).
    pub max: f64,
}

impl Agg {
    /// The time-weighted mean reading; `None` when nothing was read.
    #[inline]
    #[must_use]
    pub fn value(&self) -> Option<f64> {
        (self.tau_ms > 0 && self.mean.is_finite()).then_some(self.mean)
    }

    /// The bucket as a strip cell: its mean, its largest reading, its read time and how much of
    /// it was over.
    #[inline]
    #[must_use]
    pub fn cell(&self) -> Cell {
        let read = self.value().is_some();
        Cell {
            mean: self.value(),
            max: (read && self.max.is_finite()).then_some(self.max),
            tau_ms: if read { f64::from(self.tau_ms) } else { 0.0 },
            over_ms: if read {
                f64::from(self.over_ms.min(self.tau_ms))
            } else {
                0.0
            },
        }
    }

    /// Add reading `v` that stands for `w_ms` of the bucket, by the weighted mean update. A value
    /// that is not finite is dropped; a reading weighs at least 1 ms, so a bucket that holds one
    /// is read; the count saturates at `u16::MAX`, and the read time at `u32::MAX`, past which a
    /// reading moves only the count and the extremes (`Bucket`'s guards, A1 §1.4 item 9).
    pub fn add(&mut self, v: f64, w_ms: u32, over: bool) {
        if !v.is_finite() {
            return;
        }
        if self.n == 0 {
            (self.min, self.max) = (v, v);
        } else {
            self.min = self.min.min(v);
            self.max = self.max.max(v);
        }
        self.n = self.n.saturating_add(1);
        let tau = self.tau_ms.saturating_add(w_ms.max(1));
        let w = tau - self.tau_ms;
        if w == 0 {
            return;
        }
        self.mean = if self.tau_ms == 0 {
            v
        } else {
            self.mean + (v - self.mean) * f64::from(w) / f64::from(tau)
        };
        if over {
            self.over_ms = self.over_ms.saturating_add(w).min(tau);
        }
        self.tau_ms = tau;
    }

    /// Fold `other`, a later bucket's readings, into this one: their read times add, the means
    /// combine weighed by them, and the extremes and counts carry. An unread `other`, or one no
    /// reading could have left ([`Self::is_sound`]), changes nothing.
    pub fn merge(&mut self, other: &Self) {
        if !other.is_sound() {
            return;
        }
        if self.n == 0 {
            (self.min, self.max) = (other.min, other.max);
        } else {
            self.min = self.min.min(other.min);
            self.max = self.max.max(other.max);
        }
        self.n = self.n.saturating_add(other.n);
        let Some(m) = other.value() else {
            return;
        };
        let tau = self.tau_ms.saturating_add(other.tau_ms);
        let w = tau - self.tau_ms;
        if w == 0 {
            return;
        }
        self.mean = if self.tau_ms == 0 {
            m
        } else {
            self.mean + (m - self.mean) * f64::from(w) / f64::from(tau)
        };
        let over = u64::from(other.over_ms.min(other.tau_ms)) * u64::from(w)
            / u64::from(other.tau_ms.max(1));
        self.over_ms = self
            .over_ms
            .saturating_add(u32::try_from(over).unwrap_or(u32::MAX))
            .min(tau);
        self.tau_ms = tau;
    }

    /// Whether every field is one a reading could have left: finite extremes and mean once
    /// read, the over time within the read time. A persisted bucket that is not is dropped.
    #[must_use]
    pub fn is_sound(&self) -> bool {
        self.n > 0
            && self.tau_ms > 0
            && self.over_ms <= self.tau_ms
            && self.mean.is_finite()
            && self.min.is_finite()
            && self.max.is_finite()
            && self.min <= self.max
    }
}

/// One strip cell: a bucket, or a box of consecutive buckets or readings.
///
/// Its mean and its over share are time-weighted: each reading weighs the milliseconds it stands
/// for, so a minute half at the focused cadence and half at the background one weighs its halves
/// equally, as the strip's x does (record §5.2).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cell {
    /// The time-weighted mean of its readings, raw; `None` when none was read (hollow).
    pub mean: Option<f64>,
    /// The largest single reading: the strip box's peak tick, the lens and the accessibility text.
    pub max: Option<f64>,
    /// The read time it holds, ms: the weight its mean and its over share carry in a merge; a box
    /// that spans part of a bucket holds that part of the bucket's read time. 0 when none was
    /// read.
    pub tau_ms: f64,
    /// How much of that time was over the resource's maximum, ms.
    pub over_ms: f64,
}

impl Cell {
    /// The share of its read time that was over the resource's maximum, `0..=1`: the strip box's
    /// over cap is this long (A1 §3, ruling 8). 0 when none was read.
    #[inline]
    #[must_use]
    pub fn over_share(&self) -> f64 {
        if self.tau_ms > 0.0 && self.over_ms.is_finite() {
            (self.over_ms / self.tau_ms).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Merge consecutive cells into one box: its read time's mean, the largest max, and the read
    /// and over times summed.
    #[must_use]
    pub fn merge(cells: impl IntoIterator<Item = Cell>) -> Cell {
        Self::merge_parts(cells.into_iter().map(|c| (c, 1.0)))
    }

    /// Merge cells of which a box takes `part` (`0..=1`) each: a ten-minute bucket an hour box
    /// spans only some of brings that part of its read time, and so that part of its weight in
    /// the box's mean and its over share. A cell the box takes none of counts for nothing; one
    /// whose mean is not finite brings no time, only its max where that is finite.
    #[must_use]
    pub fn merge_parts(cells: impl IntoIterator<Item = (Cell, f64)>) -> Cell {
        let (mut sum, mut tau, mut over) = (0.0, 0.0, 0.0);
        let mut out = Cell::default();
        for (c, part) in cells {
            if part.is_nan() || part <= 0.0 {
                continue;
            }
            let w = c.tau_ms * part.min(1.0);
            if let Some(m) = c.mean.filter(|m| m.is_finite())
                && w > 0.0
            {
                sum += m * w;
                tau += w;
                over += c.over_ms.clamp(0.0, c.tau_ms) * part.min(1.0);
            }
            if let Some(x) = c.max.filter(|x| x.is_finite()) {
                out.max = Some(out.max.map_or(x, |o| o.max(x)));
            }
        }
        if tau > 0.0 {
            out.mean = Some(sum / tau);
            out.tau_ms = tau;
            out.over_ms = over;
        }
        out
    }
}

/// One reading as it was taken: when, its value, whether it was over the resource's maximum, and
/// the milliseconds it stands for.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reading {
    /// When it was taken, unix ms.
    pub at_ms: i64,
    /// Its value, raw.
    pub v: f64,
    /// Whether it was over the resource's maximum.
    pub over: bool,
    /// The milliseconds it stands for, at least 1: its slot ([`Taken::slot`]) less what the
    /// reading before it stood for ([`BucketRing::push`]). Its bucket may count less of it: a
    /// slot that crosses a bucket's edge counts only its part inside.
    pub w_ms: u32,
}

impl Reading {
    /// The reading as a strip cell of its own.
    #[inline]
    #[must_use]
    pub fn cell(self) -> Cell {
        let w = f64::from(self.w_ms.max(1));
        Cell {
            mean: Some(self.v),
            max: Some(self.v),
            tau_ms: w,
            over_ms: if self.over { w } else { 0.0 },
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
    /// Record reading `v` taken at `at_ms` (unix ms), weighing as every other reading so pushed.
    pub fn push(&mut self, at_ms: i64, v: f64) {
        self.push_reading(Reading {
            at_ms,
            v,
            over: false,
            w_ms: 1,
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

    /// Mean of readings taken after `now_ms - window_ms` through `now_ms`, each weighed by the
    /// time it stands for.
    #[must_use]
    pub fn mean(&self, now_ms: i64, window_ms: i64) -> Option<f64> {
        let cutoff = i128::from(now_ms) - i128::from(window_ms);
        let (sum, w) = self
            .v
            .iter()
            .filter(|r| i128::from(r.at_ms) > cutoff && r.at_ms <= now_ms)
            .fold((0.0, 0.0), |(s, w), r| {
                let rw = f64::from(r.w_ms.max(1));
                (s + r.v * rw, w + rw)
            });
        (w > 0.0).then(|| sum / w)
    }

    /// Largest of the readings [`Self::mean`] averages.
    #[must_use]
    pub fn max(&self, now_ms: i64, window_ms: i64) -> Option<f64> {
        let cutoff = i128::from(now_ms) - i128::from(window_ms);
        self.v
            .iter()
            .filter(|r| i128::from(r.at_ms) > cutoff && r.at_ms <= now_ms)
            .map(|r| r.v)
            .reduce(f64::max)
    }

    /// Every reading kept, oldest first.
    #[must_use]
    pub fn readings(&self) -> Vec<Reading> {
        self.v.iter().copied().collect()
    }

    /// The newest reading kept.
    #[must_use]
    pub fn newest(&self) -> Option<Reading> {
        self.v.back().copied()
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

/// Where reading slots stand on the wall clock: `slot_ms` long, each starting `origin_ms` past a
/// multiple of `slot_ms`. Readings land on the half-slot (`G/vitals_host.rs`, `due_on_phase`),
/// so every snapshot carries [`Self::SECONDS`]: the fine band reads it until A1's R7 deletes the
/// grid with the band.
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
}

/// Seven hours of ten-minute buckets, keyed by the wall clock (`at_unix_ms / LONG_BUCKET_MS`),
/// each folded from the closed ten-second buckets it holds.
#[derive(Clone, Debug, Default, PartialEq)]
struct Long {
    /// The key of `b[0]`.
    first_k: i64,
    b: VecDeque<Agg>,
    /// The start of the newest ten-second bucket folded, unix ms: the newest "reading" the
    /// back-step rule compares a fold with.
    newest_ms: Option<i64>,
}

impl Long {
    fn open_k(&self) -> Option<i64> {
        (!self.b.is_empty()).then(|| self.first_k + self.b.len() as i64 - 1)
    }

    /// Fold `agg`, the closed ten-second bucket that starts at `at_ms`, by [`lands`].
    fn fold(&mut self, at_ms: i64, agg: &Agg) {
        if agg.n == 0 {
            return;
        }
        match lands(self.open_k(), self.newest_ms, at_ms, LONG_BUCKET_MS) {
            Lands::First(k) => {
                self.first_k = k;
                self.b.push_back(Agg::default());
            }
            Lands::Newest => {}
            Lands::Ahead(by) => {
                if by >= LONG_BUCKETS as i64 {
                    self.b.clear();
                    self.first_k = at_ms.div_euclid(LONG_BUCKET_MS);
                } else {
                    for _ in 1..by {
                        self.b.push_back(Agg::default());
                    }
                }
                self.b.push_back(Agg::default());
            }
            Lands::Back(by) => {
                self.first_k -= by;
                self.newest_ms = self
                    .newest_ms
                    .map(|n| n.saturating_sub(by.saturating_mul(LONG_BUCKET_MS)));
                self.b.push_back(Agg::default());
            }
        }
        while self.b.len() > LONG_BUCKETS {
            self.b.pop_front();
            self.first_k += 1;
        }
        self.newest_ms = Some(self.newest_ms.map_or(at_ms, |n| n.max(at_ms)));
        if let Some(b) = self.b.back_mut() {
            b.merge(agg);
        }
    }
}

/// An hour of 10 s buckets and seven hours of ten-minute ones, keyed by the wall clock.
#[derive(Clone, Debug, PartialEq)]
pub struct BucketRing {
    cap: usize,
    /// The key of `b[0]`.
    first_k: i64,
    b: VecDeque<Agg>,
    /// The readings behind "now".
    recent: Recent,
    /// The ten-minute buckets the deck's 1 h boxes read.
    long: Long,
    /// Whether a closed bucket folds into `long`: not for a ring [`Self::without_long`] made.
    long_kept: bool,
    /// When the newest reading was taken, unix ms: what [`lands`] compares a reading with.
    newest_ms: Option<i64>,
    /// Where the bucket time the readings stand for ends, unix ms: a reading never weighs time an
    /// earlier one already stands for.
    covered_to: i64,
    /// Where the newest reading's slot ends, unix ms, its bucket aside: what the next reading's
    /// own weight ([`Reading::w_ms`]) starts from.
    slot_to: i64,
    /// Every ten-second key below this is in `long` already: a bucket restored or replayed
    /// folded does not fold again when it closes.
    folded_below: i64,
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
            long_kept: true,
            newest_ms: None,
            covered_to: i64::MIN,
            slot_to: i64::MIN,
            folded_below: i64::MIN,
        }
    }

    /// [`Self::with_capacity`] with no ten-minute buckets: a ring that reads only its last
    /// minute and its ten-second buckets (a deck unit's, whose six hours its row's ring carries;
    /// A1 §9 keeps no per-core 6 h rings). Its [`Self::long_cells`] stay empty.
    #[must_use]
    pub fn without_long(cap: usize) -> Self {
        Self {
            long_kept: false,
            ..Self::with_capacity(cap)
        }
    }

    /// Fold the bucket keyed `k` into the ten-minute buckets, once.
    fn fold(&mut self, k: i64) {
        if !self.long_kept || k < self.folded_below {
            return;
        }
        if let Some(agg) = self.bucket(k).copied() {
            self.long.fold(k.saturating_mul(BUCKET_MS), &agg);
        }
        self.folded_below = k.saturating_add(1);
    }

    /// Record a reading `v` taken as `t` says, over the resource's maximum where `over` says; the
    /// key of the bucket it closed, if it closed one. It lands by [`lands`], and it weighs the
    /// part of its slot ([`Taken::slot`]) inside its bucket that no earlier reading stands for.
    pub fn push(&mut self, t: Taken, v: Option<f64>, over: bool) -> Option<i64> {
        let v = v.filter(|x| x.is_finite());
        let at = t.at_ms;
        let open = self.open_k();
        let closed = match lands(open, self.newest_ms, at, BUCKET_MS) {
            Lands::First(k) => {
                self.first_k = k;
                self.b.push_back(Agg::default());
                None
            }
            Lands::Newest => None,
            Lands::Ahead(by) => {
                let was = open.unwrap_or(0);
                self.fold(was);
                if by >= self.cap as i64 {
                    self.b.clear();
                    self.first_k = at.div_euclid(BUCKET_MS);
                } else {
                    for _ in 1..by {
                        self.b.push_back(Agg::default());
                    }
                }
                self.b.push_back(Agg::default());
                self.trim();
                Some(was)
            }
            Lands::Back(by) => {
                let was = open.unwrap_or(0) - by;
                self.first_k -= by;
                self.folded_below = self.folded_below.saturating_sub(by);
                let shift = i128::from(by) * i128::from(BUCKET_MS);
                self.recent.shift(-shift);
                self.covered_to =
                    i64::try_from(i128::from(self.covered_to) - shift).unwrap_or(i64::MIN);
                self.slot_to = i64::try_from(i128::from(self.slot_to) - shift).unwrap_or(i64::MIN);
                self.newest_ms = self
                    .newest_ms
                    .and_then(|n| i64::try_from(i128::from(n) - shift).ok());
                self.fold(was);
                self.b.push_back(Agg::default());
                self.trim();
                Some(was)
            }
        };
        if self.newest_ms.is_none_or(|n| at >= n) {
            self.newest_ms = Some(at);
        }
        if let Some(v) = v {
            let k = self.open_k().unwrap_or(0);
            let (lo, hi) = t.slot();
            let b_lo = k.saturating_mul(BUCKET_MS);
            let b_hi = b_lo.saturating_add(BUCKET_MS);
            let from = lo.max(self.covered_to).max(b_lo);
            let to = hi.min(b_hi);
            let w = u32::try_from(to.saturating_sub(from).clamp(0, BUCKET_MS)).unwrap_or(0);
            if w > 0 {
                self.covered_to = to;
            }
            if let Some(b) = self.b.back_mut() {
                b.add(v, w, over);
            }
            let own = hi.saturating_sub(lo.max(self.slot_to));
            self.slot_to = self.slot_to.max(hi);
            self.recent.push_reading(Reading {
                at_ms: at,
                v,
                over,
                w_ms: u32::try_from(own.clamp(1, i64::from(u32::MAX))).unwrap_or(1),
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
    pub fn bucket(&self, k: i64) -> Option<&Agg> {
        let i = usize::try_from(k.checked_sub(self.first_k)?).ok()?;
        self.b.get(i)
    }

    /// Every bucket with its key, oldest first.
    #[inline]
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (i64, &Agg)> {
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
        self.b.iter().map(Agg::cell).collect()
    }

    /// The newest `n` buckets as cells, oldest first; the last is the open bucket.
    #[must_use]
    pub fn tail_cells(&self, n: usize) -> Vec<Cell> {
        self.b
            .iter()
            .skip(self.b.len().saturating_sub(n))
            .map(Agg::cell)
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
    /// Empty for a ring [`Self::without_long`] made.
    #[must_use]
    pub fn long_cells(&self) -> Vec<Cell> {
        self.long.b.iter().map(Agg::cell).collect()
    }

    /// The open ten-minute bucket's key (Unix ms `[600_000 * k, 600_000 * (k + 1))`); `None`
    /// before the first closed bucket folded.
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

    /// Mean of readings in the `window_ms` preceding `now_unix_ms` ([`NOW_WINDOW_MS`]), each
    /// weighed by its time.
    #[must_use]
    pub fn now_mean(&self, now_unix_ms: i64, window_ms: i64) -> Option<f64> {
        self.recent.mean(now_unix_ms, window_ms)
    }

    /// Largest of the readings [`Self::now_mean`] averages.
    #[must_use]
    pub fn now_max(&self, now_unix_ms: i64, window_ms: i64) -> Option<f64> {
        self.recent.max(now_unix_ms, window_ms)
    }

    /// Whether the open bucket holds a reading over the resource's maximum: its box draws an
    /// over cap (A1 §3).
    #[must_use]
    pub fn open_over(&self) -> bool {
        self.b.back().is_some_and(|b| b.over_ms > 0)
    }

    /// The sustained-30 s value ending at bucket `k`.
    #[must_use]
    pub fn sustained(&self, k: i64) -> Option<f64> {
        (0..3).try_fold(f64::INFINITY, |m, i| {
            self.bucket(k - i).and_then(Agg::value).map(|v| m.min(v))
        })
    }

    /// Compact snapshot of read 10 s and ten-minute buckets and last-minute readings for
    /// persistence.
    #[must_use]
    pub fn saved(&self) -> SavedRing {
        let recent = self
            .recent
            .readings()
            .into_iter()
            .filter(|r| r.v.is_finite())
            .map(|r| (r.at_ms, r.v, r.over, r.w_ms))
            .collect();
        SavedRing {
            buckets: saved_buckets(self.first_k, &self.b),
            recent,
            long: saved_buckets(self.long.first_k, &self.long.b),
            folded_below: self.folded_below,
        }
    }

    /// Restore a ring persisted in an earlier run up to `now_unix_ms`, pruning stale or future
    /// entries outside the 1-hour window (seven hours for the ten-minute buckets).
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
        ring.folded_below = saved.folded_below;
        let min_recent_ms = now_unix_ms.saturating_sub(RECENT_KEEP_MS);
        let mut recs: Vec<SavedReading> = saved
            .recent
            .iter()
            .copied()
            .filter(|&(at_ms, v, ..)| {
                v.is_finite() && at_ms > min_recent_ms && at_ms <= now_unix_ms
            })
            .collect();
        recs.sort_by_key(|&(at_ms, ..)| at_ms);
        for (at_ms, v, over, w_ms) in recs {
            ring.recent.push_reading(Reading {
                at_ms,
                v,
                over,
                w_ms: w_ms.max(1),
            });
        }
        let newest = ring.recent.newest().map(|r| r.at_ms);
        ring.newest_ms = newest;
        ring.covered_to = newest.unwrap_or(i64::MIN);
        ring.slot_to = ring.covered_to;
        ring.long.newest_ms = ring.long.open_k().map(|k| k.saturating_mul(LONG_BUCKET_MS));
        ring
    }
}

/// `b`'s read buckets, the first keyed `first_k`, as [`SavedBucket`]s.
fn saved_buckets(first_k: i64, b: &VecDeque<Agg>) -> Vec<SavedBucket> {
    b.iter()
        .enumerate()
        .filter(|(_, b)| b.is_sound())
        .map(|(i, b)| (first_k + i as i64, *b))
        .collect()
}

/// The buckets of `saved` keyed `now_k - (cap - 1)..=now_k`, as a ring's `(first_k, b)`: the
/// newest last, the unread ones between them empty, at most `cap`.
fn restored_buckets(saved: &[SavedBucket], now_k: i64, cap: usize) -> (i64, VecDeque<Agg>) {
    let min_k = now_k - (cap as i64 - 1);
    let mut valid: Vec<SavedBucket> = saved
        .iter()
        .copied()
        .filter(|&(k, b)| b.is_sound() && k >= min_k && k <= now_k)
        .collect();
    valid.sort_by_key(|&(k, _)| k);
    valid.dedup_by_key(|&mut (k, _)| k);
    let (Some(&(first, _)), Some(&(last_k, _))) = (valid.first(), valid.last()) else {
        return (0, VecDeque::new());
    };
    let len = usize::try_from(last_k - first + 1).unwrap_or(0).min(cap);
    let first_k = last_k - len as i64 + 1;
    let mut b = VecDeque::from(vec![Agg::default(); len]);
    for (k, agg) in valid {
        if let Ok(idx) = usize::try_from(k - first_k)
            && let Some(slot) = b.get_mut(idx)
        {
            *slot = agg;
        }
    }
    (first_k, b)
}

/// One persisted read bucket: its key and its readings.
pub type SavedBucket = (i64, Agg);

/// One persisted per-sample reading `(at_ms, value, over, w_ms)`.
pub type SavedReading = (i64, f64, bool, u32);

/// Compact persisted state of one [`BucketRing`].
#[derive(Clone, Debug, PartialEq)]
pub struct SavedRing {
    /// Read 10 s buckets within the hour, oldest first.
    pub buckets: Vec<SavedBucket>,
    /// Per-sample readings within the last minute, oldest first.
    pub recent: Vec<SavedReading>,
    /// Read ten-minute buckets within seven hours, oldest first, keyed by [`LONG_BUCKET_MS`].
    pub long: Vec<SavedBucket>,
    /// Every 10 s key below this is folded into `long` already.
    pub folded_below: i64,
}

impl Default for SavedRing {
    fn default() -> Self {
        Self {
            buckets: Vec::new(),
            recent: Vec::new(),
            long: Vec::new(),
            folded_below: i64::MIN,
        }
    }
}

impl SavedRing {
    /// True when neither buckets nor recent readings are held.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty() && self.recent.is_empty() && self.long.is_empty()
    }

    /// Put `agg`, the closed 10 s bucket keyed `k` that a checkpoint's tail line holds, in place
    /// of whatever bucket `k` held, and fold it into its ten-minute bucket unless the base had
    /// folded it already ([`Self::folded_below`]). A bucket the base held unfolded before `k` (the
    /// open one of a ring that read nothing since, as aterm's do while the deck is hidden) folds
    /// first, as the live ring folds its open bucket when a later one opens. An unsound `agg`
    /// changes nothing.
    pub fn put_closed(&mut self, k: i64, agg: Agg) {
        if !agg.is_sound() {
            return;
        }
        match self.buckets.binary_search_by_key(&k, |&(bk, _)| bk) {
            Ok(i) => self.buckets[i].1 = agg,
            Err(i) => self.buckets.insert(i, (k, agg)),
        }
        if k < self.folded_below {
            return;
        }
        let behind: Vec<SavedBucket> = self
            .buckets
            .iter()
            .copied()
            .filter(|&(bk, held)| bk >= self.folded_below && bk < k && held.is_sound())
            .collect();
        for (bk, held) in behind {
            self.fold_saved(bk, &held);
        }
        self.fold_saved(k, &agg);
        self.folded_below = k.saturating_add(1);
    }

    /// Fold `agg`, the 10 s bucket keyed `k`, into its ten-minute bucket.
    fn fold_saved(&mut self, k: i64, agg: &Agg) {
        let lk = k.saturating_mul(BUCKET_MS).div_euclid(LONG_BUCKET_MS);
        match self.long.binary_search_by_key(&lk, |&(bk, _)| bk) {
            Ok(i) => self.long[i].1.merge(agg),
            Err(i) => self.long.insert(i, (lk, *agg)),
        }
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
    let at = |k: i64| -> Option<Cell> {
        if k > last_k || k > now_k {
            return None;
        }
        let back = usize::try_from(i128::from(last_k) - i128::from(k)).ok()?;
        let idx = cells.len().checked_sub(1 + back)?;
        cells.get(idx).copied()
    };
    (0..ncols)
        .map(|j| {
            let steps_back = i128::from((ncols - 1 - j) as u64) * i128::from(per_b);
            let col_lo = i128::from(last_start) - steps_back;
            let col_hi = col_lo + i128::from(per_b);
            if col_hi <= i128::from(i64::MIN) || col_lo > i128::from(i64::MAX) {
                Cell::default()
            } else {
                let lo = i64::try_from(col_lo.max(i128::from(i64::MIN))).unwrap_or(i64::MIN);
                let hi_incl =
                    i64::try_from((col_hi - 1).min(i128::from(i64::MAX))).unwrap_or(i64::MAX);
                Cell::merge((lo..=hi_incl).filter_map(at))
            }
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

/// The age window, weight and band of one box in a [`MultiScaleSpec`]: the deck lays its edges
/// from the weights and bands (`crates/aterm-gui/src/stats_plan.rs`, `Strip::box_edges`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProgSlot {
    /// Older edge of the slot's window relative to `now_unix_ms` (ms ago, `>= age_lo_ms`).
    pub age_hi_ms: i64,
    /// Newer edge of the slot's window relative to `now_unix_ms` (ms ago, `0` on `live_now`).
    pub age_lo_ms: i64,
    /// Relative width weight from its [`ScaleBand`].
    pub weight: u16,
    /// True when this slot is the far-left real-time `now` box.
    pub is_live_now: bool,
    /// The index of its band in [`MultiScaleSpec::bands`]: a gate stands between two boxes of
    /// different bands.
    pub band: u8,
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
        weight: 6,
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
        weight: 9,
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
        weight: 4,
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
    ScaleBand {
        slots: 9,
        period_ms: 60_000,
        weight: 3,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 600_000,
        weight: 4,
        live_now: false,
    },
    ScaleBand {
        slots: 3,
        period_ms: 3_600_000,
        weight: 5,
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
        weight: 7,
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
        weight: 6,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 60_000,
        weight: 8,
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
        weight: 5,
        live_now: true,
    },
    ScaleBand {
        slots: 9,
        period_ms: 1_000,
        weight: 2,
        live_now: false,
    },
    ScaleBand {
        slots: 5,
        period_ms: 10_000,
        weight: 7,
        live_now: false,
    },
];

const TICKS_MINI_1M: [(i64, &str); 3] =
    [(0, "now"), (10_000, "\u{2013}10s"), (60_000, "\u{2013}1m")];

impl MultiScaleSpec {
    /// Full's six hours: the live second, nine more 1 s boxes, five of 10 s, nine of 1 min,
    /// five of 10 min and five of 1 h, each size starting where its age is its own block's
    /// (`docs/DESIGN-stats-deck-owner-2026-10-26.md` §4), 34 slots. Weighed 10; 4, 5, 6, 7, 9:
    /// from the 1 s band to the 1 h band each band's boxes are wider than the finer band's, and
    /// the live box is wider than every 1 s box (`docs/DESIGN-stats-deck-units-2026-11-04.md`
    /// §3), 205 in all.
    pub const FULL_6H: Self = Self {
        bands: &BANDS_FULL_6H,
        ticks: &TICKS_FULL_6H,
    };

    /// Four hours: [`Self::FULL_6H`]'s blocks with three of 1 h, 32 slots. Weighed 4; 1, 2, 3,
    /// 4, 5, rising from the 1 s band to the 1 h band (A1 §3), so its 1 s boxes are the narrowest
    /// it lays: about 1 pt each on a 129 pt strip, 1 device px at 1× below 208 pt.
    pub const FOLD_4H: Self = Self {
        bands: &BANDS_FOLD_4H,
        ticks: &TICKS_FOLD_4H,
    };

    /// Six minutes: the 1 s, 10 s and five 1 min blocks; too narrow for the 10 min and 1 h
    /// ones, 20 slots. Weighed 7; 4, 6, 8, rising from the 1 s band (A1 §3): on a 79 pt strip a
    /// 1 s box is about 1.9 pt.
    pub const COMPACT_6M: Self = Self {
        bands: &BANDS_COMPACT_6M,
        ticks: &TICKS_COMPACT_6M,
    };

    /// One minute: the 1 s and 10 s blocks alone, 15 slots, weighed 5; 2, 7, rising from the 1 s
    /// band (A1 §3). The 10 s band weighs enough to hold the `–10s` tick left of the strip's
    /// middle, clear of its `–1m` label at every width it lays (59–78 pt); A1's 4; 2, 3 put the
    /// tick under the label below 77 pt. On a 59 pt strip a 1 s box is about 1.4 pt.
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

    /// Every slot's age window `[age_lo_ms, age_hi_ms]`, weight and band, newest first: the live
    /// `now` box, then each older box after the last.
    #[must_use]
    pub fn slots(&self) -> Vec<ProgSlot> {
        let mut age = 0_i64;
        let mut out = Vec::with_capacity(self.slot_count());
        for (bi, band) in self.bands.iter().enumerate() {
            for _ in 0..band.slots {
                let next_age = age + band.period_ms;
                out.push(ProgSlot {
                    age_hi_ms: next_age,
                    age_lo_ms: age,
                    weight: band.weight,
                    is_live_now: band.live_now,
                    band: u8::try_from(bi).unwrap_or(u8::MAX),
                });
                age = next_age;
            }
        }
        out
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
    let ring_at = |k: i64| -> Option<Cell> {
        if k > last_k || k > now_k {
            return None;
        }
        let back = usize::try_from(i128::from(last_k) - i128::from(k)).ok()?;
        let idx = ring_cells.len().checked_sub(1 + back)?;
        ring_cells.get(idx).copied()
    };
    let long_first_wide = i128::from(tiers.long_k) + 1 - tiers.long.len() as i128;
    let long_first =
        i64::try_from(long_first_wide.clamp(i128::from(i64::MIN), i128::from(i64::MAX)))
            .unwrap_or(i64::MIN);
    let long_cell = |lo_ms: i64, hi_ms: i64| -> Cell {
        if tiers.long.is_empty() {
            return Cell::default();
        }
        let (k_lo, k_hi) = (
            lo_ms.div_euclid(LONG_BUCKET_MS),
            hi_ms
                .saturating_sub(1)
                .max(lo_ms)
                .div_euclid(LONG_BUCKET_MS),
        );
        // Each bucket brings the part of its ten minutes the box spans, so that part of its
        // readings.
        Cell::merge_parts(
            (k_lo.max(long_first)..=k_hi.min(tiers.long_k)).filter_map(|k| {
                let back = usize::try_from(i128::from(tiers.long_k) - i128::from(k)).ok()?;
                let idx = tiers.long.len().checked_sub(1 + back)?;
                let cell = tiers.long.get(idx).copied()?;
                let b_lo = i128::from(k) * i128::from(LONG_BUCKET_MS);
                let b_hi = (i128::from(k) + 1) * i128::from(LONG_BUCKET_MS);
                let ov = i128::from(hi_ms).min(b_hi) - i128::from(lo_ms).max(b_lo);
                Some((cell, ov as f64 / LONG_BUCKET_MS as f64))
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

    /// A reading at `at_ms` asked at the focused cadence.
    fn tk(at_ms: i64) -> Taken {
        Taken::at(at_ms, 1_000)
    }

    #[test]
    fn buckets_close_by_wall_clock() {
        let mut r = BucketRing::default();
        assert_eq!(r.push(tk(T0 + 9_900), Some(2.0), false), None);
        assert_eq!(
            r.push(tk(T0 + 10_000), Some(4.0), false),
            Some(T0 / BUCKET_MS),
            "a reading in the next bucket closes the open one"
        );
        assert_eq!(r.bucket(T0 / BUCKET_MS).and_then(Agg::value), Some(2.0));
        assert_eq!(r.bucket(T0 / BUCKET_MS + 1).and_then(Agg::value), Some(4.0));
        let _ = r.push(tk(T0 + 30_500), Some(6.0), false);
        let cells = r.cells();
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[2], Cell::default(), "not read is None, never 0");
        assert_eq!(cells[3].mean, Some(6.0));
        let _ = r.push(tk(T0 + 40_000), None, false);
        assert_eq!(r.cells()[4].mean, None);
    }

    #[test]
    fn the_tail_is_the_newest_buckets_with_the_open_one_last() {
        let mut r = BucketRing::default();
        assert!(r.tail_cells(3).is_empty());
        for i in [0, 1, 3, 4] {
            let _ = r.push(tk(T0 + i * 10_000), Some(i as f64), false);
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
        // The open bucket is last, its readings weighed by their time in it: 4 for the half
        // second of its slot inside the bucket, 8 for a whole second.
        let _ = r.push(tk(T0 + 45_000), Some(8.0), false);
        let last = means(r.tail_cells(1));
        assert_eq!(last.len(), 1);
        let want = (4.0 * 500.0 + 8.0 * 1_000.0) / 1_500.0;
        assert!((last[0].expect("read") - want).abs() < 1e-12, "{last:?}");
    }

    #[test]
    fn columns_merge_on_wall_clock_multiples() {
        let mut r = BucketRing::default();
        for i in 0..10 {
            let v = (i != 4).then_some(f64::from(i));
            let _ = r.push(tk(T0 + i64::from(i) * 10_000), v, i == 7);
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
        assert_eq!(
            twenty[1].over_share(),
            0.5,
            "a column's over share counts its readings: one of two was over"
        );
        assert_eq!(twenty[1].max, Some(7.0));
        assert_eq!(twenty[0].mean, Some(5.0), "the unread bucket is skipped");
        let thirty = r.columns(30, 5, now);
        assert_eq!(thirty[4].mean, Some(9.0));
        assert_eq!(thirty[3].mean, Some(7.0));
        assert_eq!(thirty[2].mean, Some(4.0));
        assert_eq!(thirty[1].mean, Some(1.0));
        assert_eq!(thirty[0], Cell::default());
        let mut sparse = BucketRing::default();
        let _ = sparse.push(tk(T0), Some(1.0), false);
        let _ = sparse.push(tk(T0 + 40_000), Some(1.0), false);
        let cols = sparse.columns(20, 3, T0 + 40_000);
        assert_eq!(cols[1].mean, None);
        assert_eq!(cols[0].mean, Some(1.0));
        assert_eq!(columns_of(&[], i64::MAX, 10, 2, i64::MAX).len(), 2);
        assert_eq!(
            columns_of(&[Cell::default()], i64::MIN, 10, 4, i64::MIN).len(),
            4
        );
    }

    #[test]
    fn unknown_is_never_zero_in_a_bucket_a_column_or_now() {
        let mut r = BucketRing::default();
        for i in 0..6 {
            let _ = r.push(tk(T0 + i * 10_000), None, false);
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
        assert_eq!(Agg::default().value(), None);
        assert_eq!(Agg::default().cell().over_share(), 0.0);
        let _ = r.push(tk(T0 + 60_000), Some(0.0), false);
        assert_eq!(r.now_mean(T0 + 60_000, NOW_WINDOW_MS), Some(0.0));
        assert_eq!(r.cells().last().and_then(|c| c.mean), Some(0.0));
    }

    #[test]
    fn clock_steps_back() {
        let mut r = BucketRing::default();
        for i in 0..6 {
            let _ = r.push(tk(T0 + i * 10_000), Some(i as f64), false);
        }
        let open = r.open_k().expect("pushed");
        assert_eq!(r.push(tk(T0 + 25_000), Some(5.0), false), None);
        assert_eq!(r.open_k(), Some(open));
        assert_eq!(r.bucket(open).map(|b| b.n), Some(2));
        let before: Vec<Cell> = r.cells();
        let back = T0 + 25_000 - 2 * 3_600_000;
        let closed = r.push(tk(back), Some(9.0), false);
        let k = back.div_euclid(BUCKET_MS);
        assert_eq!(r.open_k(), Some(k));
        assert_eq!(closed, Some(k - 1));
        let after = r.cells();
        assert_eq!(&after[..before.len()], &before[..]);
        assert_eq!(after.last().and_then(|c| c.mean), Some(9.0));
        assert_eq!(r.now_mean(back, NOW_WINDOW_MS), Some(9.0));
        let _ = r.push(tk(back + 40_000), Some(1.0), false);
        let cells = r.cells();
        let n = cells.len();
        assert_eq!(cells[n - 4..n - 1], [Cell::default(); 3]);
        assert_eq!(cells[n - 1].mean, Some(1.0));
        let mut extremes = BucketRing::default();
        let _ = extremes.push(tk(i64::MAX), Some(1.0), false);
        let _ = extremes.push(tk(i64::MIN), Some(2.0), false);
        assert_eq!(extremes.open_k(), Some(i64::MIN.div_euclid(BUCKET_MS)));
        assert_eq!(extremes.now_mean(i64::MIN, NOW_WINDOW_MS), Some(2.0));
    }

    #[test]
    fn the_ring_keeps_an_hour_and_a_long_silence_is_unread() {
        let mut r = BucketRing::default();
        for i in 0..400 {
            let _ = r.push(tk(T0 + i * 10_000), Some(1.0), false);
        }
        assert_eq!(r.len(), RING_BUCKETS);
        assert_eq!(r.open_k(), Some(T0 / BUCKET_MS + 399));
        let _ = r.push(tk(T0 + 399 * 10_000 + 2 * 3_600_000), Some(1.0), false);
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
        // Readings on the half-slot, each standing for its own second.
        let mut r = BucketRing::default();
        for i in 0..15 {
            let _ = r.push(tk(T0 + i * 1000 + 500), Some(i as f64), false);
        }
        assert_eq!(r.now_mean(T0 + 14_500, NOW_WINDOW_MS), Some(9.5));
        assert_eq!(
            r.bucket(T0 / BUCKET_MS + 1).and_then(Agg::value),
            Some(12.0)
        );
    }

    #[test]
    fn the_sustained_value_needs_three_read_buckets_and_ignores_one_spike() {
        let mut r = BucketRing::default();
        for (i, v) in [5.0, 6.0, 90.0, 7.0, 6.0].into_iter().enumerate() {
            let _ = r.push(tk(T0 + i as i64 * 10_000), Some(v), false);
        }
        let k0 = T0 / BUCKET_MS;
        assert_eq!(r.sustained(k0 + 1), None, "only two buckets before it");
        assert_eq!(r.sustained(k0 + 2), Some(5.0));
        assert_eq!(r.sustained(k0 + 3), Some(6.0), "the spike is not sustained");
        assert_eq!(r.sustained(k0 + 4), Some(6.0));
    }

    #[test]
    fn a_ring_keeps_its_readings_with_their_over_marks() {
        let mut r = BucketRing::default();
        let _ = r.push(tk(T0), Some(1.0), false);
        let _ = r.push(tk(T0 + 1_000), None, false);
        let _ = r.push(tk(T0 + 2_000), Some(3.0), true);
        assert_eq!(
            r.readings(),
            [
                Reading {
                    at_ms: T0,
                    v: 1.0,
                    over: false,
                    w_ms: 1_000,
                },
                Reading {
                    at_ms: T0 + 2_000,
                    v: 3.0,
                    over: true,
                    w_ms: 1_000,
                },
            ],
            "an unread reading is no reading"
        );
        // The minute it keeps, and no more.
        let _ = r.push(tk(T0 + 2_000 + RECENT_KEEP_MS), Some(4.0), false);
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
            assert!(live.is_live_now && live.age_lo_ms == 0);
            assert!(slots[1..].iter().all(|s| !s.is_live_now));
            assert_eq!(slots.last().unwrap().age_hi_ms, spec.horizon_ms());
            for pair in slots.windows(2) {
                assert_eq!(
                    pair[0].age_hi_ms, pair[1].age_lo_ms,
                    "the boxes tile the age"
                );
            }
            for pair in slots.windows(2) {
                assert!(
                    pair[0].band == pair[1].band || pair[0].band + 1 == pair[1].band,
                    "each slot names its band, in order: {pair:?}"
                );
                assert_eq!(
                    pair[0].band == pair[1].band,
                    pair[0].age_hi_ms - pair[0].age_lo_ms == pair[1].age_hi_ms - pair[1].age_lo_ms
                        && pair[0].is_live_now == pair[1].is_live_now,
                    "a band is its boxes' period: {pair:?}"
                );
            }
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

    /// Seven hours of ten-minute buckets (`LONG_BUCKETS` 43, record §5.2): the 1 h box leaving
    /// the 6 h horizon still has its children. Each folds the closed ten-second buckets it holds,
    /// as they close.
    #[test]
    fn a_ring_keeps_seven_hours_of_ten_minute_buckets_and_restores_them() {
        let mut r = BucketRing::default();
        assert_eq!((r.long_k(), r.long_cells().len()), (None, 0));
        let t0 = T0 - T0.rem_euclid(LONG_BUCKET_MS);
        for i in 0..8 * 360 {
            let at = t0 + i * 10_000;
            let _ = r.push(
                tk(at),
                Some(if at < t0 + 3_000_000 { 1.0 } else { 3.0 }),
                false,
            );
        }
        let k_now = (t0 + (8 * 360 - 1) * 10_000).div_euclid(LONG_BUCKET_MS);
        assert_eq!(r.long_k(), Some(k_now));
        let cells = r.long_cells();
        assert_eq!(cells.len(), LONG_BUCKETS, "seven hours and the open bucket");
        assert!(
            cells.iter().all(|c| c.mean == Some(3.0)),
            "the first fifty minutes aged out"
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
        // The open ten-second bucket is not folded yet: the newest ten-minute bucket holds the
        // 59 closed before it, each one reading at its start (half a second of it).
        let newest = r.long_cells().last().copied().expect("read");
        assert_eq!(newest.tau_ms, 59.0 * 500.0);
    }

    /// Record §5.2 (`folded_below`): a ring restored from a base whose newest bucket a tail line
    /// already folded folds it no second time when the next reading closes it, so its ten-minute
    /// bucket holds the 20 s actually read. CONTROL: the same restore with its fold mark lowered
    /// to that bucket, as the mark would stand were it not kept, folds it again, 30 s for 20.
    #[test]
    fn a_restored_ring_folds_its_newest_bucket_once() {
        let t0 = T0 - T0.rem_euclid(LONG_BUCKET_MS);
        let mut r = BucketRing::default();
        for i in 0..20 {
            let _ = r.push(tk(t0 + 500 + i * 1_000), Some(1.0), false);
        }
        let k1 = t0 / BUCKET_MS + 1;
        assert_eq!(r.open_k(), Some(k1));
        let mut saved = r.saved();
        saved.put_closed(k1, *r.bucket(k1).expect("the second bucket"));
        assert_eq!(saved.folded_below, k1 + 1);
        let now = t0 + 20_500;
        let long_tau = |saved: &SavedRing| {
            let mut back = BucketRing::restored(saved, now);
            let _ = back.push(tk(now), Some(1.0), false);
            back.long_cells().last().map(|c| c.tau_ms)
        };
        assert_eq!(
            long_tau(&saved),
            Some(20_000.0),
            "20 s read, each folded once"
        );
        let unmarked = SavedRing {
            folded_below: k1,
            ..saved
        };
        assert_eq!(
            long_tau(&unmarked),
            Some(30_000.0),
            "CONTROL: an unkept mark folds the restored bucket twice"
        );
    }

    /// F4′: the 10 s now-mean, the live box's column and the row's %, weighs each reading by the
    /// time it stands for: a 5 s background reading of 0 beside five focused readings of 10 is
    /// half each. Negative control: `Recent::mean` weighing each reading alike (the mutant the
    /// F4′ review ran) gives the count-weighted 50/6, which the scenario keeps apart from 5.
    #[test]
    fn the_now_mean_weighs_each_reading_by_its_time() {
        let mut r = BucketRing::default();
        let _ = r.push(Taken::at(T0 + 2_500, 5_000), Some(0.0), false);
        for i in 0..5 {
            let _ = r.push(Taken::at(T0 + 5_500 + i * 1_000, 1_000), Some(10.0), false);
        }
        let now = T0 + 9_500;
        let mean = r.now_mean(now, NOW_WINDOW_MS).expect("read");
        assert!((mean - 5.0).abs() < 1e-12, "five seconds of each: {mean}");
        let readings = r.readings();
        let count = readings.iter().map(|x| x.v).sum::<f64>() / readings.len() as f64;
        assert!(
            (count - 50.0 / 6.0).abs() < 1e-12 && (count - mean).abs() > 3.0,
            "the scenario: its count-weighted mean {count} stands apart"
        );
    }

    /// A1 §1.4 item 9 (`lands`): a ten-minute history re-keyed by a fold more than
    /// [`CLAMP_BACK_S`] behind moves its newest reading back with its buckets, so a fold 10 s
    /// behind the re-keyed newest, across a ten-minute edge, lands in the newest bucket as the
    /// clamp says. CONTROL: a fold 70 s behind, past the clamp, re-keys again.
    #[test]
    fn a_rekeyed_long_clamps_the_next_fold_by_its_moved_newest() {
        let e = T0 - T0.rem_euclid(LONG_BUCKET_MS) + 10 * LONG_BUCKET_MS;
        let ek = e / LONG_BUCKET_MS;
        let agg = |v: f64| {
            let mut a = Agg::default();
            a.add(v, 10_000, false);
            a
        };
        let mut long = Long::default();
        long.fold(e + 290_000, &agg(1.0));
        long.fold(e - LONG_BUCKET_MS, &agg(2.0));
        assert_eq!(
            (long.first_k, long.open_k()),
            (ek - 2, Some(ek - 1)),
            "fifteen minutes back re-keys"
        );
        long.fold(e - LONG_BUCKET_MS - BUCKET_MS, &agg(3.0));
        assert_eq!(
            (long.first_k, long.open_k()),
            (ek - 2, Some(ek - 1)),
            "10 s behind the moved newest lands in it"
        );
        assert_eq!(long.b.back().map(|a| (a.n, a.max)), Some((2, 3.0)));
        let mut past = long.clone();
        past.fold(e - LONG_BUCKET_MS - 70_000, &agg(4.0));
        assert_eq!(
            (past.first_k, past.open_k()),
            (ek - 4, Some(ek - 2)),
            "CONTROL: 70 s behind re-keys"
        );
    }

    #[test]
    fn hour_boxes_read_the_ten_minute_buckets_by_how_much_of_each_they_span() {
        let mut r = BucketRing::default();
        let t0 = T0 - T0.rem_euclid(LONG_BUCKET_MS);
        for i in 0..6 * 360 {
            let at = t0 + i * 10_000;
            let v = (at.div_euclid(LONG_BUCKET_MS) - t0 / LONG_BUCKET_MS) as f64;
            let _ = r.push(tk(at), Some(v), false);
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
        let _ = ring.push(tk(now - 90_000), Some(20.0), false);
        let _ = ring.push(tk(now - 10_000), Some(50.0), false);
        let _ = ring.push(tk(now - 1_000), Some(80.0), false);
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
                let _ = ring.push(tk(T0 + t), Some(1.0), false);
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
            let _ = earlier.push(tk(T0 + t), Some(2.0), false);
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
        assert_eq!(
            multi_scale_cells(
                Tiers {
                    ring: &[Cell::default()],
                    ring_k: i64::MAX,
                    long: &[Cell::default()],
                    long_k: i64::MAX.div_euclid(LONG_BUCKET_MS),
                    recent: &[],
                },
                i64::MAX,
                MultiScaleSpec::FULL_6H,
                2_500,
                &[],
            )
            .len(),
            MultiScaleSpec::FULL_6H.slot_count()
        );
    }

    /// A1 §1.4 item 9: `Agg` keeps `Bucket`'s guards. A value that is not finite is dropped; the
    /// count saturates at `u16::MAX` and the read time at `u32::MAX`, past which a reading moves
    /// only the count and the extremes; a merge brings nothing from a bucket no reading could
    /// have left. `Cell::merge` and `Cell::merge_parts` ignore non-finite cells. CONTROL: the full
    /// bucket with the count a wrapping add leaves (0) is one `is_sound` refuses, so a save and a
    /// merge would drop it.
    #[test]
    fn agg_drops_non_finite_and_saturates_its_count() {
        let mut a = Agg::default();
        a.add(f64::NAN, 1_000, true);
        a.add(f64::INFINITY, 1_000, true);
        a.add(f64::NEG_INFINITY, 1_000, false);
        assert_eq!(a, Agg::default(), "non-finite readings are dropped");
        a.add(0.5, 0, false);
        assert_eq!((a.n, a.tau_ms), (1, 1), "a reading weighs at least 1 ms");

        let mut full = Agg {
            tau_ms: u32::MAX - 500,
            n: u16::MAX - 1,
            over_ms: 0,
            mean: 0.5,
            min: 0.5,
            max: 0.5,
        };
        full.add(0.9, 1_000, true);
        assert_eq!(full.n, u16::MAX);
        assert_eq!(full.tau_ms, u32::MAX);
        assert_eq!(full.over_ms, 500, "only the time it could still hold");
        full.add(0.1, 1_000, true);
        assert_eq!(
            (full.n, full.tau_ms, full.over_ms),
            (u16::MAX, u32::MAX, 500)
        );
        assert_eq!((full.min, full.max), (0.1, 0.9), "the extremes still carry");
        let mean = full.mean;
        full.add(1.0, 1_000, false);
        assert_eq!(full.mean, mean, "a saturated read time moves no mean");
        assert!(full.is_sound());
        let wrapped = Agg {
            n: (u16::MAX - 1).wrapping_add(2),
            ..full
        };
        let mut into = Agg::default();
        into.merge(&wrapped);
        assert!(
            !wrapped.is_sound() && into == Agg::default(),
            "CONTROL: a count that wrapped to 0 makes the full bucket one a save and a merge drop"
        );

        let mut m = Agg::default();
        m.add(0.25, 4_000, true);
        let before = m;
        m.merge(&Agg {
            tau_ms: 3_000,
            n: 3,
            over_ms: 3_000,
            mean: f64::NAN,
            min: f64::NAN,
            max: f64::INFINITY,
        });
        assert_eq!(
            m, before,
            "a merge drops a bucket no reading could have left"
        );

        let nan_cell = Cell {
            mean: Some(f64::NAN),
            max: Some(f64::INFINITY),
            tau_ms: 3_000.0,
            over_ms: 3_000.0,
        };
        let finite_cell = Cell {
            mean: Some(0.25),
            max: Some(0.75),
            tau_ms: 4_000.0,
            over_ms: 4_000.0,
        };
        assert_eq!(Cell::merge([nan_cell, finite_cell]), finite_cell);
        assert_eq!(
            Cell::merge_parts([(nan_cell, 0.5), (finite_cell, 1.0)]),
            finite_cell
        );
    }

    /// A box's mean and over share weigh each bucket by its read time (A1 §3, time-weighted since
    /// F4′): a bucket read for 10 s at 0.2 and one read for 2 s at 0.8 merge to 0.3, not the
    /// buckets' 0.5; and an hour box that spans half of a ten-minute bucket takes half of that
    /// bucket's read time and half of its over time. FULL_6H's newest hour box, at a now halfway
    /// through a ten-minute bucket, spans half of its oldest bucket (read 10 min at 0.9, all
    /// over), five whole ones (read 5 min each at 0.3) and half of its newest (5 min at 0.3):
    /// 32.5 min read, 5 over, mean 153 / 390. CONTROL: weighing each bucket by its part alone
    /// reads 0.35, and counting the half bucket's over time whole reads 120 / 390.
    #[test]
    fn a_merge_weighs_each_reading_by_its_time_and_a_part_brings_its_share_of_the_over() {
        let cell = |mean: f64, tau_ms: f64, over_ms: f64| Cell {
            mean: Some(mean),
            max: Some(mean),
            tau_ms,
            over_ms,
        };
        let two = Cell::merge([cell(0.2, 10_000.0, 0.0), cell(0.8, 2_000.0, 0.0)]);
        assert!((two.mean.unwrap() - 0.3).abs() < 1e-12, "{two:?}");
        assert_eq!((two.tau_ms, two.max), (12_000.0, Some(0.8)));
        assert!(
            (two.mean.unwrap() - 0.5).abs() > 0.1,
            "CONTROL: not the buckets' mean"
        );

        let k0 = T0.div_euclid(LONG_BUCKET_MS);
        assert_eq!(
            T0.rem_euclid(LONG_BUCKET_MS),
            0,
            "CONTROL: T0 opens a bucket"
        );
        let now = T0 + 7 * LONG_BUCKET_MS + LONG_BUCKET_MS / 2;
        let long_k = now.div_euclid(LONG_BUCKET_MS);
        let long: Vec<Cell> = (long_k + 1 - LONG_BUCKETS as i64..=long_k)
            .map(|k| {
                if k == k0 - 5 {
                    cell(0.9, 600_000.0, 600_000.0)
                } else {
                    cell(0.3, 300_000.0, 0.0)
                }
            })
            .collect();
        let tiers = Tiers {
            long: &long,
            long_k,
            ..Tiers::default()
        };
        let cells = multi_scale_cells(tiers, now, MultiScaleSpec::FULL_6H, 2_500, &[]);
        let hour = cells
            .iter()
            .find(|c| c.slot.age_lo_ms == RING_REACH_MS)
            .expect("the newest hour box");
        assert_eq!(
            (now - hour.slot.age_hi_ms, now - hour.slot.age_lo_ms),
            (
                (k0 - 5) * LONG_BUCKET_MS + LONG_BUCKET_MS / 2,
                (k0 + 1) * LONG_BUCKET_MS + LONG_BUCKET_MS / 2
            ),
            "CONTROL: the box spans half of its edge buckets"
        );
        let got = hour.cell;
        assert!((got.tau_ms - 1_950_000.0).abs() < 1e-6, "{got:?}");
        assert!((got.over_ms - 300_000.0).abs() < 1e-6, "{got:?}");
        assert!((got.over_share() - 60.0 / 390.0).abs() < 1e-12, "{got:?}");
        assert!((got.mean.unwrap() - 153.0 / 390.0).abs() < 1e-12, "{got:?}");
        assert_eq!(got.max, Some(0.9));
        assert!(
            (got.mean.unwrap() - 0.35).abs() > 0.02
                && (got.over_share() - 120.0 / 390.0).abs() > 0.1,
            "CONTROL: the count and the part each move the box"
        );
    }

    /// A1 §3's weights: from the 1 s band to the 1 h band each band's boxes weigh strictly more
    /// than the finer band's, in every preset, so with gates and cumulative rounding no box of a
    /// coarser band lays narrower than one of a finer band; the live box weighs more than a 1 s
    /// box. FULL_6H weighs 10; 4, 5, 6, 7, 9 (205 in all). CONTROL: the presets main laid before
    /// A1 (FOLD_4H's 2; 1, 1, 1, 2, 2) do not rise.
    #[test]
    fn band_weights_rise_strictly_from_the_one_second_band_to_the_oldest_in_every_preset() {
        let rises = |bands: &[ScaleBand]| {
            let history: Vec<&ScaleBand> = bands.iter().filter(|b| !b.live_now).collect();
            let live = bands.iter().find(|b| b.live_now);
            history.windows(2).all(|w| w[0].weight < w[1].weight)
                && live
                    .zip(history.first())
                    .is_some_and(|(l, f)| l.weight > f.weight)
        };
        for spec in [
            MultiScaleSpec::FULL_6H,
            MultiScaleSpec::FOLD_4H,
            MultiScaleSpec::COMPACT_6M,
            MultiScaleSpec::MINI_1M,
        ] {
            assert!(rises(spec.bands), "{:?}", spec.bands);
            assert!(
                spec.bands
                    .windows(2)
                    .all(|w| w[0].period_ms <= w[1].period_ms),
                "bands run from now outward"
            );
        }
        assert_eq!(MultiScaleSpec::FULL_6H.total_weight(), 205);
        let weights =
            |spec: MultiScaleSpec| spec.bands.iter().map(|b| b.weight).collect::<Vec<_>>();
        assert_eq!(weights(MultiScaleSpec::FULL_6H), [10, 4, 5, 6, 7, 9]);
        assert_eq!(weights(MultiScaleSpec::FOLD_4H), [4, 1, 2, 3, 4, 5]);
        assert_eq!(weights(MultiScaleSpec::COMPACT_6M), [7, 4, 6, 8]);
        assert_eq!(weights(MultiScaleSpec::MINI_1M), [5, 2, 7]);
        let before = [
            (1, 2, true),
            (9, 1, false),
            (5, 1, false),
            (9, 1, false),
            (5, 2, false),
        ]
        .map(|(slots, weight, live_now)| ScaleBand {
            slots,
            period_ms: 1_000,
            weight,
            live_now,
        });
        assert!(!rises(&before), "CONTROL: equal weights do not rise");
    }

    /// Ruling 8 (A1 §3): over is a proportional share at every horizon. A four-minute over spell,
    /// read once a second, shows in every box that holds any of it while it ages from the 1 min
    /// band through the 10 min band into the hour band, each box's share its over time over its
    /// read time, so in an hour box it is about 4/60 and never rounds away. CONTROL: under the
    /// half-the-readings flag a bucket or box is over only where half its readings were, so the
    /// hour box holding the spell would read not over.
    #[test]
    fn an_over_spell_shows_in_every_box_that_holds_it_at_every_horizon() {
        let t0 = T0 - T0.rem_euclid(LONG_BUCKET_MS);
        let (spell_lo, spell_hi) = (t0 + 2 * 3_600_000, t0 + 2 * 3_600_000 + 240_000);
        let mut horizons = Vec::new();
        for age in [120_000_i64, 1_800_000, 3 * 3_600_000 - 240_000] {
            let now = spell_hi + age;
            let mut r = BucketRing::default();
            for at in (t0..=now).step_by(1_000) {
                let over = (spell_lo..spell_hi).contains(&at);
                let _ = r.push(tk(at), Some(if over { 0.99 } else { 0.3 }), over);
            }
            let (ring, long, recent) = (r.cells(), r.long_cells(), r.readings());
            let tiers = Tiers {
                ring: &ring,
                ring_k: r.open_k().unwrap_or(0),
                long: &long,
                long_k: r.long_k().unwrap_or(0),
                recent: &recent,
            };
            let cells = multi_scale_cells(tiers, now, MultiScaleSpec::FULL_6H, 2_500, &[]);
            let holding: Vec<&ScaleCell> = cells
                .iter()
                .filter(|c| {
                    let (lo, hi) = (now - c.slot.age_hi_ms, now - c.slot.age_lo_ms);
                    lo < spell_hi && hi > spell_lo
                })
                .collect();
            assert!(!holding.is_empty(), "{age} ms on: a box holds the spell");
            for c in &holding {
                let (lo, hi) = (now - c.slot.age_hi_ms, now - c.slot.age_lo_ms);
                // A box past the last minute and inside the hour reads the whole 10 s buckets
                // it touches; an hour box the part of each ten-minute bucket it spans.
                let (lo_r, hi_r) = if c.slot.age_lo_ms >= RING_REACH_MS {
                    (lo, hi)
                } else {
                    (
                        lo.div_euclid(BUCKET_MS) * BUCKET_MS,
                        ((hi - 1).div_euclid(BUCKET_MS) + 1) * BUCKET_MS,
                    )
                };
                let inside = (hi_r.min(spell_hi) - lo_r.max(spell_lo)).max(0) as f64;
                let want = inside / (hi_r - lo_r) as f64;
                let got = c.cell.over_share();
                assert!(got > 0.0, "{age} ms on: {c:?} shows the spell");
                let period = c.slot.age_hi_ms - c.slot.age_lo_ms;
                assert!(
                    (got - want).abs() <= 1e-3,
                    "{age} ms on: {got} against {want}: {c:?}"
                );
                if period >= 3_600_000 {
                    // CONTROL: what main drew before A1. An hour box was over where one of its
                    // ten-minute buckets was over for half its readings, and the spell is 4 of
                    // its bucket's 10 minutes.
                    let first = r.long_k().unwrap_or(0) + 1 - long.len() as i64;
                    let half_rule = (lo.div_euclid(LONG_BUCKET_MS)
                        ..=(hi - 1).div_euclid(LONG_BUCKET_MS))
                        .filter_map(|k| long.get(usize::try_from(k - first).ok()?))
                        .any(|b| b.tau_ms > 0.0 && b.over_ms * 2.0 >= b.tau_ms);
                    assert!(
                        !half_rule,
                        "CONTROL: the half rule erases the spell from {c:?}"
                    );
                }
            }
            horizons.push(
                holding
                    .iter()
                    .map(|c| c.slot.age_hi_ms - c.slot.age_lo_ms)
                    .max(),
            );
        }
        assert_eq!(
            horizons,
            [Some(60_000), Some(600_000), Some(3_600_000)],
            "CONTROL: the spell was seen in the 1 min, 10 min and 1 h bands"
        );
    }

    /// Record §5.2: weights are time, not readings. A minute read half at the focused cadence
    /// (1.0, a reading a second) and half in the background (0.0, a reading each 5 s), each on
    /// its half-slot, reads 0.5 in its 1 min box, because the strip's x is time and each half is
    /// 30 s of it. CONTROL: counting readings, its 30 readings at 1.0 and 6 at 0.0 read 0.83.
    #[test]
    fn coarse_boxes_weigh_their_buckets_by_time_not_by_reading_count() {
        let mut r = BucketRing::default();
        for i in 0..30 {
            let _ = r.push(Taken::at(T0 + i * 1_000 + 500, 1_000), Some(1.0), false);
        }
        for j in 0..6 {
            let _ = r.push(
                Taken::at(T0 + 30_000 + j * 5_000 + 2_500, 5_000),
                Some(0.0),
                false,
            );
        }
        let _ = r.push(Taken::at(T0 + 60_000 + 2_500, 5_000), Some(0.0), false);
        let now = T0 + 120_000;
        let (ring, recent) = (r.cells(), r.readings());
        let tiers = Tiers {
            ring: &ring,
            ring_k: r.open_k().unwrap_or(0),
            recent: &recent,
            ..Tiers::default()
        };
        let cells = multi_scale_cells(tiers, now, MultiScaleSpec::FULL_6H, 2_500, &[]);
        let minute = cells
            .iter()
            .find(|c| c.slot.age_lo_ms == 60_000 && c.slot.age_hi_ms == 120_000)
            .expect("the 1 min band's newest box");
        assert_eq!(minute.cell.tau_ms, 60_000.0, "every ms read once");
        assert!(
            (minute.cell.mean.expect("read") - 0.5).abs() < 1e-12,
            "{minute:?}"
        );
        let (n, sum) = r
            .iter()
            .filter(|(k, _)| (T0 / BUCKET_MS..T0 / BUCKET_MS + 6).contains(k))
            .fold((0.0, 0.0), |(n, sum), (_, b)| {
                (n + f64::from(b.n), sum + f64::from(b.n) * b.mean)
            });
        assert!(
            (sum / n - 30.0 / 36.0).abs() < 1e-12,
            "CONTROL: {}",
            sum / n
        );
    }

    /// Record §5.2: the 1 h box leaving the 6 h horizon still has its children. An hour box's
    /// epoch can stand an hour past the sixth (A1 §6.2's pour steps the hour bands once an
    /// hour), so the ring keeps every ten-minute bucket of the seven hours before now, and the
    /// open one. Negative control: `LONG_BUCKETS` 37, as main kept, fails the leaving hour's
    /// first bucket (the mutant the F4′ review ran); the scenario's leaving bucket is one 37
    /// buckets would not hold.
    #[test]
    fn the_horizons_leaving_box_keeps_its_children() {
        let mut r = BucketRing::default();
        let t0 = T0 - T0.rem_euclid(LONG_BUCKET_MS);
        let mut now = t0;
        for i in 0..9 * 360 {
            now = t0 + i * 10_000 + 5_000;
            let _ = r.push(Taken::at(now, 10_000), Some(1.0), false);
        }
        let long = r.long_cells();
        let long_k = r.long_k().expect("read");
        let first = long_k + 1 - long.len() as i64;
        let leaving = (now - 7 * 3_600_000).div_euclid(LONG_BUCKET_MS);
        for k in leaving..=(now - 6 * 3_600_000).div_euclid(LONG_BUCKET_MS) {
            let cell = usize::try_from(k - first)
                .ok()
                .and_then(|i| long.get(i))
                .copied()
                .unwrap_or_default();
            assert_eq!(cell.mean, Some(1.0), "bucket {k} of the leaving hour");
        }
        assert!(
            long_k + 1 - 37 > leaving,
            "the scenario: 37 buckets would start after the leaving one"
        );
    }

    /// Record §5.2: a reading late past its slot is never counted twice in a coarse box. Each
    /// reading weighs only the part of its slot that no earlier reading stands for, so however
    /// the readings jitter a bucket's read time is the union of their slots inside it, and never
    /// more than the bucket; a late reading still reaches the strip's finest boxes, with the time
    /// it stands for. (Its borrow of the slot it missed is the display's, `SLOT_COVER_MS`, A1's
    /// R7.) CONTROL: summing each reading's whole slot counts the jittered time twice.
    #[test]
    fn a_late_reading_borrows_for_display_and_counts_once() {
        let jitter = [0, 400, 950, 300, -200, 0, 700, 100, 0, 300];
        let times: Vec<i64> = jitter
            .iter()
            .enumerate()
            .map(|(i, j)| T0 + i as i64 * 1_000 + 500 + j)
            .collect();
        let mut r = BucketRing::default();
        for &t in &times {
            let _ = r.push(tk(t), Some(1.0), false);
        }
        let b = *r.bucket(T0 / BUCKET_MS).expect("read");
        let mut union: Vec<(i64, i64)> = times
            .iter()
            .map(|&t| tk(t).slot())
            .map(|(lo, hi)| (lo.max(T0), hi.min(T0 + BUCKET_MS)))
            .collect();
        union.sort_unstable();
        let (mut covered, mut to) = (0, i64::MIN);
        for (lo, hi) in union {
            covered += (hi - lo.max(to)).max(0);
            to = to.max(hi);
        }
        assert_eq!(i64::from(b.tau_ms), covered);
        assert!(b.tau_ms <= 10_000);
        assert_eq!(b.n, 10);
        let late = r
            .readings()
            .into_iter()
            .find(|x| x.at_ms == T0 + 2_000 + 500 + 950)
            .expect("the late reading reaches the finest boxes");
        assert!(late.w_ms >= 1);
        let naive: i64 = times
            .iter()
            .map(|&t| tk(t).slot())
            .map(|(lo, hi)| hi.min(T0 + BUCKET_MS) - lo.max(T0))
            .sum();
        assert!(naive > covered, "CONTROL: {naive} against {covered}");
    }
}
