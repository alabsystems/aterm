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
        (w > 0.0).then_some(sum / w)
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
        let mut out = Vec::with_capacity(self.slot_count());
        out.extend(self.iter_slots());
        out
    }

    /// Stream the same slots as [`Self::slots`] without allocating their storage.
    #[inline]
    pub fn iter_slots(self) -> impl Iterator<Item = ProgSlot> + Clone {
        self.bands
            .iter()
            .enumerate()
            .flat_map(|(bi, band)| (0..band.slots).map(move |_| (bi, band)))
            .scan(0_i64, |age, (bi, band)| {
                let next_age = *age + band.period_ms;
                let slot = ProgSlot {
                    age_hi_ms: next_age,
                    age_lo_ms: *age,
                    weight: band.weight,
                    is_live_now: band.live_now,
                    band: u8::try_from(bi).unwrap_or(u8::MAX),
                };
                *age = next_age;
                Some(slot)
            })
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
#[path = "ring_tests.rs"]
mod tests;
